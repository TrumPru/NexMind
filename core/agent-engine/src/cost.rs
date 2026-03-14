use std::collections::HashMap;
use std::sync::Arc;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use ulid::Ulid;

use nexmind_event_bus::EventBus;
use nexmind_storage::Database;

use crate::AgentError;

/// Pricing for a single model (USD per 1K tokens).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelPricing {
    pub input_per_1k: f64,
    pub output_per_1k: f64,
}

/// Price table mapping model IDs to pricing.
#[derive(Debug, Clone)]
pub struct PriceTable {
    pub prices: HashMap<String, ModelPricing>,
}

impl Default for PriceTable {
    fn default() -> Self {
        let mut prices = HashMap::new();
        // Anthropic
        prices.insert(
            "anthropic/claude-sonnet-4-20250514".into(),
            ModelPricing {
                input_per_1k: 0.003,
                output_per_1k: 0.015,
            },
        );
        prices.insert(
            "anthropic/claude-haiku-4-5-20251001".into(),
            ModelPricing {
                input_per_1k: 0.0008,
                output_per_1k: 0.004,
            },
        );
        // OpenAI
        prices.insert(
            "openai/gpt-4o".into(),
            ModelPricing {
                input_per_1k: 0.0025,
                output_per_1k: 0.01,
            },
        );
        prices.insert(
            "openai/gpt-4o-mini".into(),
            ModelPricing {
                input_per_1k: 0.00015,
                output_per_1k: 0.0006,
            },
        );
        // Ollama / local models: free
        prices.insert(
            "ollama/llama3.2".into(),
            ModelPricing {
                input_per_1k: 0.0,
                output_per_1k: 0.0,
            },
        );
        PriceTable { prices }
    }
}

impl PriceTable {
    /// Look up pricing for a model. Unknown models are treated as free.
    pub fn get_pricing(&self, model: &str) -> ModelPricing {
        self.prices
            .get(model)
            .cloned()
            .unwrap_or(ModelPricing {
                input_per_1k: 0.0,
                output_per_1k: 0.0,
            })
    }

    /// Calculate cost in USD for a given model and token counts.
    pub fn calculate_cost(&self, model: &str, input_tokens: u32, output_tokens: u32) -> f64 {
        let pricing = self.get_pricing(model);
        let input_cost = (input_tokens as f64 / 1000.0) * pricing.input_per_1k;
        let output_cost = (output_tokens as f64 / 1000.0) * pricing.output_per_1k;
        input_cost + output_cost
    }

    /// Calculate cost in microdollars (1_000_000 = $1.00).
    pub fn calculate_cost_microdollars(
        &self,
        model: &str,
        input_tokens: u32,
        output_tokens: u32,
    ) -> i64 {
        let cost_usd = self.calculate_cost(model, input_tokens, output_tokens);
        (cost_usd * 1_000_000.0) as i64
    }
}

/// A single cost record to store.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CostRecord {
    pub workspace_id: String,
    pub agent_id: String,
    pub run_id: String,
    pub model: String,
    pub provider: String,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cost_microdollars: i64,
}

/// Summary of costs over a period.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CostSummary {
    pub total_cost_usd: f64,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub total_requests: u64,
    pub by_model: HashMap<String, f64>,
    pub by_agent: HashMap<String, f64>,
}

/// Time period for cost queries.
#[derive(Debug, Clone, Copy)]
pub enum CostPeriod {
    Today,
    Last7Days,
    Last30Days,
    AllTime,
}

impl CostPeriod {
    fn sql_filter(&self) -> &str {
        match self {
            CostPeriod::Today => "timestamp >= datetime('now', '-1 day')",
            CostPeriod::Last7Days => "timestamp >= datetime('now', '-7 days')",
            CostPeriod::Last30Days => "timestamp >= datetime('now', '-30 days')",
            CostPeriod::AllTime => "1=1",
        }
    }
}

/// Budget check result.
#[derive(Debug, Clone, PartialEq)]
pub enum BudgetStatus {
    Ok,
    Warning { pct_used: f64 },
    Exceeded,
}

/// Cost tracker — records and queries cost data.
pub struct CostTracker {
    db: Arc<Database>,
    price_table: PriceTable,
    #[allow(dead_code)]
    event_bus: Arc<EventBus>,
}

impl CostTracker {
    pub fn new(db: Arc<Database>, event_bus: Arc<EventBus>) -> Self {
        Self {
            db,
            price_table: PriceTable::default(),
            event_bus,
        }
    }

    pub fn with_price_table(mut self, price_table: PriceTable) -> Self {
        self.price_table = price_table;
        self
    }

    /// Get a reference to the price table.
    pub fn price_table(&self) -> &PriceTable {
        &self.price_table
    }

    /// Record a completed LLM call.
    pub fn record(&self, record: CostRecord) -> Result<(), AgentError> {
        let id = format!("cost_{}", Ulid::new());
        let now = Utc::now().to_rfc3339();

        // Derive provider from model string (e.g., "anthropic/claude-..." -> "anthropic")
        let provider = if record.provider.is_empty() {
            record
                .model
                .split('/')
                .next()
                .unwrap_or("unknown")
                .to_string()
        } else {
            record.provider.clone()
        };

        let conn = self
            .db
            .conn()
            .map_err(|e| AgentError::StorageError(e.to_string()))?;
        conn.execute(
            "INSERT INTO cost_records (id, timestamp, workspace_id, agent_id, run_id, model, provider, input_tokens, output_tokens, cost_microdollars) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            rusqlite::params![
                id,
                now,
                record.workspace_id,
                record.agent_id,
                record.run_id,
                record.model,
                provider,
                record.input_tokens as i64,
                record.output_tokens as i64,
                record.cost_microdollars,
            ],
        )
        .map_err(|e| AgentError::StorageError(e.to_string()))?;

        Ok(())
    }

    /// Get cost summary for a workspace over a time period.
    pub fn summary(
        &self,
        workspace_id: &str,
        period: CostPeriod,
    ) -> Result<CostSummary, AgentError> {
        let conn = self
            .db
            .conn()
            .map_err(|e| AgentError::StorageError(e.to_string()))?;

        let time_filter = period.sql_filter();
        let query = format!(
            "SELECT COALESCE(SUM(cost_microdollars), 0), COALESCE(SUM(input_tokens), 0), COALESCE(SUM(output_tokens), 0), COUNT(*) FROM cost_records WHERE workspace_id = ?1 AND {}",
            time_filter
        );

        let (total_micro, total_in, total_out, count): (i64, i64, i64, i64) = conn
            .query_row(&query, rusqlite::params![workspace_id], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })
            .map_err(|e| AgentError::StorageError(e.to_string()))?;

        // By model
        let by_model_query = format!(
            "SELECT model, SUM(cost_microdollars) FROM cost_records WHERE workspace_id = ?1 AND {} GROUP BY model",
            time_filter
        );
        let mut stmt = conn
            .prepare(&by_model_query)
            .map_err(|e| AgentError::StorageError(e.to_string()))?;
        let by_model: HashMap<String, f64> = stmt
            .query_map(rusqlite::params![workspace_id], |row| {
                let model: String = row.get(0)?;
                let micro: i64 = row.get(1)?;
                Ok((model, micro as f64 / 1_000_000.0))
            })
            .map_err(|e| AgentError::StorageError(e.to_string()))?
            .filter_map(|r| r.ok())
            .collect();

        // By agent
        let by_agent_query = format!(
            "SELECT agent_id, SUM(cost_microdollars) FROM cost_records WHERE workspace_id = ?1 AND {} GROUP BY agent_id",
            time_filter
        );
        let mut stmt = conn
            .prepare(&by_agent_query)
            .map_err(|e| AgentError::StorageError(e.to_string()))?;
        let by_agent: HashMap<String, f64> = stmt
            .query_map(rusqlite::params![workspace_id], |row| {
                let agent: String = row.get(0)?;
                let micro: i64 = row.get(1)?;
                Ok((agent, micro as f64 / 1_000_000.0))
            })
            .map_err(|e| AgentError::StorageError(e.to_string()))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(CostSummary {
            total_cost_usd: total_micro as f64 / 1_000_000.0,
            total_input_tokens: total_in as u64,
            total_output_tokens: total_out as u64,
            total_requests: count as u64,
            by_model,
            by_agent,
        })
    }

    /// Get cost for a specific agent over a period.
    pub fn agent_cost(
        &self,
        agent_id: &str,
        period: CostPeriod,
    ) -> Result<CostSummary, AgentError> {
        let conn = self
            .db
            .conn()
            .map_err(|e| AgentError::StorageError(e.to_string()))?;

        let time_filter = period.sql_filter();
        let query = format!(
            "SELECT COALESCE(SUM(cost_microdollars), 0), COALESCE(SUM(input_tokens), 0), COALESCE(SUM(output_tokens), 0), COUNT(*) FROM cost_records WHERE agent_id = ?1 AND {}",
            time_filter
        );

        let (total_micro, total_in, total_out, count): (i64, i64, i64, i64) = conn
            .query_row(&query, rusqlite::params![agent_id], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })
            .map_err(|e| AgentError::StorageError(e.to_string()))?;

        // By model
        let by_model_query = format!(
            "SELECT model, SUM(cost_microdollars) FROM cost_records WHERE agent_id = ?1 AND {} GROUP BY model",
            time_filter
        );
        let mut stmt = conn
            .prepare(&by_model_query)
            .map_err(|e| AgentError::StorageError(e.to_string()))?;
        let by_model: HashMap<String, f64> = stmt
            .query_map(rusqlite::params![agent_id], |row| {
                let model: String = row.get(0)?;
                let micro: i64 = row.get(1)?;
                Ok((model, micro as f64 / 1_000_000.0))
            })
            .map_err(|e| AgentError::StorageError(e.to_string()))?
            .filter_map(|r| r.ok())
            .collect();

        Ok(CostSummary {
            total_cost_usd: total_micro as f64 / 1_000_000.0,
            total_input_tokens: total_in as u64,
            total_output_tokens: total_out as u64,
            total_requests: count as u64,
            by_model,
            by_agent: HashMap::new(),
        })
    }

    /// Check if an agent's budget is exceeded for a given run.
    /// Uses per-day cost against max_cost_per_day_usd and per-run against max_cost_per_run_usd.
    pub fn check_budget(
        &self,
        agent_id: &str,
        budget: &crate::definition::BudgetPolicy,
    ) -> Result<BudgetStatus, AgentError> {
        let conn = self
            .db
            .conn()
            .map_err(|e| AgentError::StorageError(e.to_string()))?;

        // Get today's cost for this agent
        let today_cost_micro: i64 = conn
            .query_row(
                "SELECT COALESCE(SUM(cost_microdollars), 0) FROM cost_records WHERE agent_id = ?1 AND timestamp >= datetime('now', '-1 day')",
                rusqlite::params![agent_id],
                |row| row.get(0),
            )
            .map_err(|e| AgentError::StorageError(e.to_string()))?;

        let today_cost_usd = today_cost_micro as f64 / 1_000_000.0;

        if today_cost_usd >= budget.max_cost_per_day_usd {
            return Ok(BudgetStatus::Exceeded);
        }

        let pct_used = today_cost_usd / budget.max_cost_per_day_usd * 100.0;
        if pct_used >= 80.0 {
            return Ok(BudgetStatus::Warning { pct_used });
        }

        Ok(BudgetStatus::Ok)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (Arc<Database>, Arc<EventBus>, CostTracker) {
        let db = Database::open_in_memory().unwrap();
        db.run_migrations().unwrap();
        let db = Arc::new(db);
        let bus = Arc::new(EventBus::with_default_capacity());
        let tracker = CostTracker::new(db.clone(), bus.clone());
        (db, bus, tracker)
    }

    #[test]
    fn test_price_table_known_model() {
        let pt = PriceTable::default();
        let pricing = pt.get_pricing("anthropic/claude-sonnet-4-20250514");
        assert_eq!(pricing.input_per_1k, 0.003);
        assert_eq!(pricing.output_per_1k, 0.015);
    }

    #[test]
    fn test_price_table_unknown_model_is_free() {
        let pt = PriceTable::default();
        let pricing = pt.get_pricing("some/unknown-model");
        assert_eq!(pricing.input_per_1k, 0.0);
        assert_eq!(pricing.output_per_1k, 0.0);
    }

    #[test]
    fn test_calculate_cost() {
        let pt = PriceTable::default();
        // 2000 input tokens + 800 output tokens of claude-sonnet-4
        let cost = pt.calculate_cost("anthropic/claude-sonnet-4-20250514", 2000, 800);
        // 2.0 * 0.003 + 0.8 * 0.015 = 0.006 + 0.012 = 0.018
        assert!((cost - 0.018).abs() < 0.0001);
    }

    #[test]
    fn test_calculate_cost_microdollars() {
        let pt = PriceTable::default();
        let micro = pt.calculate_cost_microdollars("anthropic/claude-sonnet-4-20250514", 2000, 800);
        assert_eq!(micro, 18000); // $0.018 = 18000 microdollars
    }

    #[test]
    fn test_record_and_summary() {
        let (_db, _bus, tracker) = setup();

        tracker
            .record(CostRecord {
                workspace_id: "ws_test".into(),
                agent_id: "agt_1".into(),
                run_id: "run_1".into(),
                model: "anthropic/claude-sonnet-4-20250514".into(),
                provider: "anthropic".into(),
                input_tokens: 1000,
                output_tokens: 500,
                cost_microdollars: 10500,
            })
            .unwrap();

        tracker
            .record(CostRecord {
                workspace_id: "ws_test".into(),
                agent_id: "agt_1".into(),
                run_id: "run_2".into(),
                model: "openai/gpt-4o".into(),
                provider: "openai".into(),
                input_tokens: 2000,
                output_tokens: 1000,
                cost_microdollars: 15000,
            })
            .unwrap();

        let summary = tracker.summary("ws_test", CostPeriod::AllTime).unwrap();
        assert_eq!(summary.total_requests, 2);
        assert_eq!(summary.total_input_tokens, 3000);
        assert_eq!(summary.total_output_tokens, 1500);
        assert!((summary.total_cost_usd - 0.0255).abs() < 0.0001);
        assert_eq!(summary.by_model.len(), 2);
        assert_eq!(summary.by_agent.len(), 1);
    }

    #[test]
    fn test_agent_cost() {
        let (_db, _bus, tracker) = setup();

        tracker
            .record(CostRecord {
                workspace_id: "ws_test".into(),
                agent_id: "agt_a".into(),
                run_id: "run_1".into(),
                model: "anthropic/claude-sonnet-4-20250514".into(),
                provider: "anthropic".into(),
                input_tokens: 1000,
                output_tokens: 500,
                cost_microdollars: 10500,
            })
            .unwrap();

        tracker
            .record(CostRecord {
                workspace_id: "ws_test".into(),
                agent_id: "agt_b".into(),
                run_id: "run_2".into(),
                model: "openai/gpt-4o".into(),
                provider: "openai".into(),
                input_tokens: 2000,
                output_tokens: 1000,
                cost_microdollars: 15000,
            })
            .unwrap();

        let cost_a = tracker.agent_cost("agt_a", CostPeriod::AllTime).unwrap();
        assert_eq!(cost_a.total_requests, 1);
        assert_eq!(cost_a.total_input_tokens, 1000);

        let cost_b = tracker.agent_cost("agt_b", CostPeriod::AllTime).unwrap();
        assert_eq!(cost_b.total_requests, 1);
        assert_eq!(cost_b.total_input_tokens, 2000);
    }

    #[test]
    fn test_budget_check_ok() {
        let (_db, _bus, tracker) = setup();

        let budget = crate::definition::BudgetPolicy {
            max_tokens_per_run: 100_000,
            max_cost_per_run_usd: 1.0,
            max_cost_per_day_usd: 10.0,
        };

        // No records yet - should be OK
        let status = tracker.check_budget("agt_1", &budget).unwrap();
        assert_eq!(status, BudgetStatus::Ok);
    }

    #[test]
    fn test_budget_check_warning() {
        let (_db, _bus, tracker) = setup();

        let budget = crate::definition::BudgetPolicy {
            max_tokens_per_run: 100_000,
            max_cost_per_run_usd: 1.0,
            max_cost_per_day_usd: 0.01, // $0.01 daily budget
        };

        // Record $0.0085 (85% of $0.01)
        tracker
            .record(CostRecord {
                workspace_id: "ws_test".into(),
                agent_id: "agt_1".into(),
                run_id: "run_1".into(),
                model: "test".into(),
                provider: "test".into(),
                input_tokens: 100,
                output_tokens: 50,
                cost_microdollars: 8500, // $0.0085
            })
            .unwrap();

        let status = tracker.check_budget("agt_1", &budget).unwrap();
        match status {
            BudgetStatus::Warning { pct_used } => {
                assert!(pct_used >= 80.0);
            }
            other => panic!("expected Warning, got {:?}", other),
        }
    }

    #[test]
    fn test_budget_check_exceeded() {
        let (_db, _bus, tracker) = setup();

        let budget = crate::definition::BudgetPolicy {
            max_tokens_per_run: 100_000,
            max_cost_per_run_usd: 1.0,
            max_cost_per_day_usd: 0.01, // $0.01 daily budget
        };

        // Record $0.02 (200% of $0.01)
        tracker
            .record(CostRecord {
                workspace_id: "ws_test".into(),
                agent_id: "agt_1".into(),
                run_id: "run_1".into(),
                model: "test".into(),
                provider: "test".into(),
                input_tokens: 100,
                output_tokens: 50,
                cost_microdollars: 20000, // $0.02
            })
            .unwrap();

        let status = tracker.check_budget("agt_1", &budget).unwrap();
        assert_eq!(status, BudgetStatus::Exceeded);
    }
}
