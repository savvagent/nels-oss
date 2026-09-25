use rmcp::{
    model::{
        CallToolResult, ContentBlock,
        ListResourcesResult, ReadResourceResponse, ReadResourceResult, ReadResourceRequestParams, Resource, ResourceContents,
        PaginatedRequestParams, ErrorData,
    },
    tool, tool_router,
    handler::server::ServerHandler,
    handler::server::wrapper::Parameters,
    service::RequestContext,
    schemars::JsonSchema,
    transport::streamable_http_server::tower::{StreamableHttpService, StreamableHttpServerConfig},
    transport::streamable_http_server::session::local::LocalSessionManager,
};
use serde::Deserialize;
use uuid::Uuid;
use axum::{
    extract::{State, Extension, Query, Path},
    response::IntoResponse,
    http::Request,
    body::Body,
};
use std::sync::Arc;
use chrono::{NaiveDate, Utc};

use crate::auth::AppState;
use crate::budget::{
    check_permission, Permission, BudgetPayload, ListBudgetsQuery, CategoryPayload, CategoryUpdatePayload,
    TransactionPayload, TransactionUpdatePayload, create_budget, list_budgets, update_budget,
    close_budget, archive_budget, unarchive_budget, link_rollup, unlink_rollup,
    create_category, update_category, enable_category_fund, disable_category_fund,
    create_transaction, list_transactions, update_transaction, delete_transaction,
    categories_view,
};
use crate::retirement::{get_profile, upsert_profile, build_projection_request, RetirementProfileInput, ProjectionOverrides};
use crate::retirement_projection::run_projection;
use crate::assets::list_assets;
use crate::goals::{create_goal, CreateGoalPayload};
use crate::notifications::{create_reminder_handler, CreateReminderPayload};

#[derive(Clone)]
pub struct NelsMcpServer {
    pub state: AppState,
}

impl NelsMcpServer {
    pub fn new(state: AppState) -> Self {
        Self { state }
    }

    async fn extract_user_id(&self, ctx: &RequestContext<rmcp::RoleServer>) -> Result<Uuid, ErrorData> {
        let parts = ctx.extensions.get::<axum::http::request::Parts>().ok_or_else(|| {
            ErrorData::internal_error("Missing request context", None)
        })?;
        parts.extensions.get::<Uuid>().copied().ok_or_else(|| {
            ErrorData::internal_error("Unauthenticated request", None)
        })
    }

    pub async fn resolve_target_budget(&self, user_id: Uuid, requested: Option<Uuid>) -> Result<Uuid, ErrorData> {
        if let Some(bid) = requested {
            let perm = check_permission(&self.state.db, user_id, bid)
                .await
                .map_err(|e| ErrorData::internal_error(e, None))?;
            if perm != Permission::None {
                return Ok(bid);
            } else {
                return Err(ErrorData::invalid_params("No permission for specified budget", None));
            }
        }

        let preferred: Option<Uuid> = sqlx::query_scalar("SELECT active_budget_id FROM users WHERE id = $1")
            .bind(user_id)
            .fetch_optional(&self.state.db)
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?
            .flatten();

        if let Some(bid) = preferred {
            let perm = check_permission(&self.state.db, user_id, bid)
                .await
                .map_err(|e| ErrorData::internal_error(e, None))?;
            if perm != Permission::None {
                return Ok(bid);
            }
        }

        let default_bid: Option<Uuid> = sqlx::query_scalar("SELECT id FROM budgets WHERE owner_id = $1 AND is_default = TRUE")
            .bind(user_id)
            .fetch_optional(&self.state.db)
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;

        if let Some(bid) = default_bid {
            return Ok(bid);
        }

        let first_bid: Option<Uuid> = sqlx::query_scalar("SELECT id FROM budgets WHERE owner_id = $1 ORDER BY created_at ASC LIMIT 1")
            .bind(user_id)
            .fetch_optional(&self.state.db)
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;

        first_bid.ok_or_else(|| ErrorData::invalid_params("No active budget found. Please create a budget first.", None))
    }

    // Direct user-scoped handlers shared between MCP protocol and Nels built-in chat
    pub async fn create_budget_for_user(&self, user_id: Uuid, params: CreateBudgetParams) -> Result<CallToolResult, ErrorData> {
        let payload = BudgetPayload {
            name: params.name,
            description: params.description,
            time_frame: params.time_frame.unwrap_or_else(|| "monthly".to_string()),
            budget_limit: params.budget_limit,
            rollover_enabled: params.rollover_enabled,
            budget_type: params.budget_type,
            auto_renew: params.auto_renew,
            amount_mode: params.amount_mode,
            budget_strategy: params.budget_strategy,
            currency: params.currency,
        };

        let res = create_budget(State(self.state.clone()), Extension(user_id), axum::Json(payload)).await
            .map_err(|(_code, msg)| ErrorData::invalid_params(msg, None))?;

        let json = serde_json::to_string_pretty(&res.0).unwrap_or_default();
        Ok(CallToolResult::success(vec![ContentBlock::text(json)]))
    }

    pub async fn list_budgets_for_user(&self, user_id: Uuid) -> Result<CallToolResult, ErrorData> {
        let res = list_budgets(State(self.state.clone()), Query(ListBudgetsQuery { archived: None }), Extension(user_id)).await
            .map_err(|(_code, msg)| ErrorData::internal_error(msg, None))?;

        let json = serde_json::to_string_pretty(&res.0).unwrap_or_default();
        Ok(CallToolResult::success(vec![ContentBlock::text(json)]))
    }

    pub async fn update_budget_for_user(&self, user_id: Uuid, params: UpdateBudgetParams) -> Result<CallToolResult, ErrorData> {
        let budget_id = self.resolve_target_budget(user_id, params.budget_id).await?;

        let payload = BudgetPayload {
            name: params.name,
            description: params.description,
            time_frame: params.time_frame,
            budget_limit: params.budget_limit,
            rollover_enabled: params.rollover_enabled,
            budget_type: params.budget_type,
            auto_renew: params.auto_renew,
            amount_mode: params.amount_mode,
            budget_strategy: params.budget_strategy,
            currency: params.currency,
        };

        let res = update_budget(State(self.state.clone()), Path(budget_id), Extension(user_id), axum::Json(payload)).await
            .map_err(|(_code, msg)| ErrorData::invalid_params(msg, None))?;

        let json = serde_json::to_string_pretty(&res.0).unwrap_or_default();
        Ok(CallToolResult::success(vec![ContentBlock::text(json)]))
    }

    pub async fn close_budget_for_user(&self, user_id: Uuid, params: BudgetIdParams) -> Result<CallToolResult, ErrorData> {
        let res = close_budget(State(self.state.clone()), Path(params.budget_id), Extension(user_id)).await
            .map_err(|(_code, msg)| ErrorData::invalid_params(msg, None))?;

        let json = serde_json::to_string_pretty(&res.0).unwrap_or_default();
        Ok(CallToolResult::success(vec![ContentBlock::text(json)]))
    }

    pub async fn archive_budget_for_user(&self, user_id: Uuid, params: BudgetIdParams) -> Result<CallToolResult, ErrorData> {
        let res = archive_budget(State(self.state.clone()), Path(params.budget_id), Extension(user_id)).await
            .map_err(|(_code, msg)| ErrorData::invalid_params(msg, None))?;

        let json = serde_json::to_string_pretty(&res.0).unwrap_or_default();
        Ok(CallToolResult::success(vec![ContentBlock::text(json)]))
    }

    pub async fn unarchive_budget_for_user(&self, user_id: Uuid, params: BudgetIdParams) -> Result<CallToolResult, ErrorData> {
        let res = unarchive_budget(State(self.state.clone()), Path(params.budget_id), Extension(user_id)).await
            .map_err(|(_code, msg)| ErrorData::invalid_params(msg, None))?;

        let json = serde_json::to_string_pretty(&res.0).unwrap_or_default();
        Ok(CallToolResult::success(vec![ContentBlock::text(json)]))
    }

    pub async fn rollup_budget_for_user(&self, user_id: Uuid, params: RollupParams) -> Result<CallToolResult, ErrorData> {
        let payload = crate::budget::RollupPayload { child_budget_id: params.child_budget_id };
        let res = link_rollup(State(self.state.clone()), Path(params.parent_budget_id), Extension(user_id), axum::Json(payload)).await
            .map_err(|(_code, msg)| ErrorData::invalid_params(msg, None))?;

        let json = serde_json::to_string_pretty(&res.0).unwrap_or_default();
        Ok(CallToolResult::success(vec![ContentBlock::text(json)]))
    }

    pub async fn unrollup_budget_for_user(&self, user_id: Uuid, params: RollupParams) -> Result<CallToolResult, ErrorData> {
        let _ = unlink_rollup(State(self.state.clone()), Path((params.parent_budget_id, params.child_budget_id)), Extension(user_id)).await
            .map_err(|(_code, msg)| ErrorData::invalid_params(msg, None))?;

        Ok(CallToolResult::success(vec![ContentBlock::text("Successfully unrolled budget")]))
    }

    pub async fn add_transaction_for_user(&self, user_id: Uuid, params: AddTransactionParams) -> Result<CallToolResult, ErrorData> {
        let budget_id = self.resolve_target_budget(user_id, params.budget_id).await?;

        let payload = TransactionPayload {
            category_id: params.category_id,
            amount: params.amount,
            description: params.description,
            transaction_date: None,
        };

        let res = create_transaction(State(self.state.clone()), Path(budget_id), Extension(user_id), axum::Json(payload)).await
            .map_err(|(_code, msg)| ErrorData::invalid_params(msg, None))?;

        let json = serde_json::to_string_pretty(&res.0).unwrap_or_default();
        Ok(CallToolResult::success(vec![ContentBlock::text(json)]))
    }

    pub async fn list_transactions_for_user(&self, user_id: Uuid, params: ListTransactionsParams) -> Result<CallToolResult, ErrorData> {
        let budget_id = self.resolve_target_budget(user_id, params.budget_id).await?;

        let res = list_transactions(State(self.state.clone()), Path(budget_id), Extension(user_id)).await
            .map_err(|(_code, msg)| ErrorData::internal_error(msg, None))?;

        let json = serde_json::to_string_pretty(&res.0).unwrap_or_default();
        Ok(CallToolResult::success(vec![ContentBlock::text(json)]))
    }

    pub async fn edit_transaction_for_user(&self, user_id: Uuid, params: EditTransactionParams) -> Result<CallToolResult, ErrorData> {
        let budget_id = self.resolve_target_budget(user_id, params.budget_id).await?;

        let payload = TransactionUpdatePayload {
            category_id: params.category_id,
            amount: params.amount,
            description: params.description,
            transaction_date: None,
            excluded_from_budget: params.excluded_from_budget,
        };

        let res = update_transaction(State(self.state.clone()), Path((budget_id, params.transaction_id)), Extension(user_id), axum::Json(payload)).await
            .map_err(|(_code, msg)| ErrorData::invalid_params(msg, None))?;

        let json = serde_json::to_string_pretty(&res.0).unwrap_or_default();
        Ok(CallToolResult::success(vec![ContentBlock::text(json)]))
    }

    pub async fn delete_transaction_for_user(&self, user_id: Uuid, params: DeleteTransactionParams) -> Result<CallToolResult, ErrorData> {
        let budget_id = self.resolve_target_budget(user_id, params.budget_id).await?;

        let _ = delete_transaction(State(self.state.clone()), Path((budget_id, params.transaction_id)), Extension(user_id)).await
            .map_err(|(_code, msg)| ErrorData::invalid_params(msg, None))?;

        Ok(CallToolResult::success(vec![ContentBlock::text("Successfully deleted transaction")]))
    }

    pub async fn create_category_for_user(&self, user_id: Uuid, params: CreateCategoryParams) -> Result<CallToolResult, ErrorData> {
        let budget_id = self.resolve_target_budget(user_id, params.budget_id).await?;

        let payload = CategoryPayload {
            name: params.name,
            category_type: params.category_type,
            category_limit: params.category_limit,
            rollover_enabled: params.rollover_enabled,
        };

        let res = create_category(State(self.state.clone()), Path(budget_id), Extension(user_id), axum::Json(payload)).await
            .map_err(|(_code, msg)| ErrorData::invalid_params(msg, None))?;

        let json = serde_json::to_string_pretty(&res.0).unwrap_or_default();
        Ok(CallToolResult::success(vec![ContentBlock::text(json)]))
    }

    pub async fn update_category_for_user(&self, user_id: Uuid, params: UpdateCategoryParams) -> Result<CallToolResult, ErrorData> {
        let budget_id = self.resolve_target_budget(user_id, params.budget_id).await?;

        let payload = CategoryUpdatePayload {
            name: params.name,
            category_type: params.category_type,
            category_limit: params.category_limit,
            rollover_enabled: params.rollover_enabled,
            is_fund: params.is_fund,
        };

        let res = update_category(State(self.state.clone()), Path((budget_id, params.category_id)), Extension(user_id), axum::Json(payload)).await
            .map_err(|(_code, msg)| ErrorData::invalid_params(msg, None))?;

        let json = serde_json::to_string_pretty(&res.0).unwrap_or_default();
        Ok(CallToolResult::success(vec![ContentBlock::text(json)]))
    }

    pub async fn set_category_fund_for_user(&self, user_id: Uuid, params: SetCategoryFundParams) -> Result<CallToolResult, ErrorData> {
        let budget_id = self.resolve_target_budget(user_id, params.budget_id).await?;

        let time_frame: String = sqlx::query_scalar("SELECT time_frame FROM budgets WHERE id = $1")
            .bind(budget_id)
            .fetch_optional(&self.state.db)
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?
            .unwrap_or_else(|| "monthly".to_string());

        if params.is_fund {
            enable_category_fund(&self.state.db, params.category_id, &time_frame).await
                .map_err(|e| ErrorData::invalid_params(e.to_string(), None))?;
        } else {
            disable_category_fund(&self.state.db, params.category_id).await
                .map_err(|e| ErrorData::invalid_params(e.to_string(), None))?;
        }

        Ok(CallToolResult::success(vec![ContentBlock::text(format!(
            "Set fund status to {} for category {}", params.is_fund, params.category_id
        ))]))
    }

    pub async fn set_retirement_profile_for_user(&self, user_id: Uuid, params: SetRetirementProfileParams) -> Result<CallToolResult, ErrorData> {
        let input = RetirementProfileInput {
            country: params.country,
            birth_date: params.birth_date.and_then(|d| NaiveDate::parse_from_str(&d, "%Y-%m-%d").ok()),
            current_gross_income: params.current_gross_income,
            target_retirement_age: params.target_retirement_age,
            contribution_rate_pre_tax: params.contribution_rate_pre_tax,
            contribution_rate_roth: params.contribution_rate_roth,
            expected_real_return: params.expected_real_return,
            inflation_rate: params.inflation_rate,
            target_replacement_ratio: params.target_replacement_ratio,
            life_expectancy_age: params.life_expectancy_age,
            employer_match_formula: None,
            ss_monthly_benefit: params.ss_monthly_benefit,
            ss_benefit_at_age: params.ss_benefit_at_age.map(crate::retirement::SsAnchor::Age),
            ss_claiming_age: params.ss_claiming_age.map(crate::retirement::SsAnchor::Age),
        };

        let (profile, warning) = upsert_profile(&self.state.db, user_id, &input, Utc::now().date_naive()).await
            .map_err(|(_code, msg)| ErrorData::invalid_params(msg, None))?;

        let mut res_map = serde_json::to_value(&profile).unwrap_or_default();
        if let Some(w) = warning {
            if let serde_json::Value::Object(ref mut map) = res_map {
                map.insert("warning".to_string(), serde_json::Value::String(w));
            }
        }

        Ok(CallToolResult::success(vec![ContentBlock::text(serde_json::to_string_pretty(&res_map).unwrap_or_default())]))
    }

    pub async fn run_retirement_projection_for_user(&self, user_id: Uuid, params: RetirementProjectionParams) -> Result<CallToolResult, ErrorData> {
        let overrides = ProjectionOverrides {
            target_retirement_age: params.target_retirement_age_override,
            contribution_rate_pre_tax: params.contribution_rate_pre_tax_override,
            contribution_rate_roth: params.contribution_rate_roth_override,
            expected_real_return: params.expected_real_return_override,
            current_gross_income: None,
            target_replacement_ratio: None,
            effective_tax_rate: None,
            employer_match_formula: None,
            ss_claiming_age_months: None,
        };

        let req = build_projection_request(&self.state.db, user_id, Utc::now().date_naive(), &overrides).await
            .map_err(|(_code, msg)| ErrorData::invalid_params(msg, None))?;

        let proj = run_projection(&req);
        match proj {
            Ok(p) => {
                let json = serde_json::to_string_pretty(&p).unwrap_or_default();
                Ok(CallToolResult::success(vec![ContentBlock::text(json)]))
            }
            Err(e) => Err(ErrorData::invalid_params(format!("Projection failed: {e:?}"), None)),
        }
    }

    pub async fn list_assets_for_user(&self, user_id: Uuid) -> Result<CallToolResult, ErrorData> {
        let res = list_assets(State(self.state.clone()), Extension(user_id)).await
            .map_err(|(_code, msg)| ErrorData::internal_error(msg, None))?;

        let json = serde_json::to_string_pretty(&res.0).unwrap_or_default();
        Ok(CallToolResult::success(vec![ContentBlock::text(json)]))
    }

    pub async fn create_goal_for_user(&self, user_id: Uuid, params: CreateGoalParams) -> Result<CallToolResult, ErrorData> {
        let budget_id = self.resolve_target_budget(user_id, params.budget_id).await?;

        let target_date = params.target_date.and_then(|d| NaiveDate::parse_from_str(&d, "%Y-%m-%d").ok());
        let payload = CreateGoalPayload {
            name: params.name,
            goal_type: params.goal_type,
            target_amount: params.target_amount,
            target_date,
            linked_category_id: None,
        };

        let res = create_goal(State(self.state.clone()), Path(budget_id), Extension(user_id), axum::Json(payload)).await
            .map_err(|(_code, msg)| ErrorData::invalid_params(msg, None))?;

        let json = serde_json::to_string_pretty(&res.0).unwrap_or_default();
        Ok(CallToolResult::success(vec![ContentBlock::text(json)]))
    }

    pub async fn create_reminder_for_user(&self, user_id: Uuid, params: CreateReminderParams) -> Result<CallToolResult, ErrorData> {
        let payload = CreateReminderPayload {
            message: params.message,
            cadence: params.cadence,
            budget_id: params.budget_id,
        };

        let res = create_reminder_handler(State(self.state.clone()), Extension(user_id), axum::Json(payload)).await
            .map_err(|(_code, msg)| ErrorData::invalid_params(msg, None))?;

        let json = serde_json::to_string_pretty(&res.0).unwrap_or_default();
        Ok(CallToolResult::success(vec![ContentBlock::text(json)]))
    }
}

// Params Structs

#[derive(Deserialize, JsonSchema)]
pub struct CreateBudgetParams {
    pub name: String,
    pub description: Option<String>,
    pub time_frame: Option<String>,
    pub budget_limit: Option<f64>,
    pub rollover_enabled: Option<bool>,
    pub budget_type: Option<String>,
    pub auto_renew: Option<bool>,
    pub amount_mode: Option<String>,
    pub budget_strategy: Option<String>,
    pub currency: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct UpdateBudgetParams {
    pub budget_id: Option<Uuid>,
    pub name: String,
    pub description: Option<String>,
    pub time_frame: String,
    pub budget_limit: Option<f64>,
    pub rollover_enabled: Option<bool>,
    pub budget_type: Option<String>,
    pub auto_renew: Option<bool>,
    pub amount_mode: Option<String>,
    pub budget_strategy: Option<String>,
    pub currency: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct BudgetIdParams {
    pub budget_id: Uuid,
}

#[derive(Deserialize, JsonSchema)]
pub struct RollupParams {
    pub parent_budget_id: Uuid,
    pub child_budget_id: Uuid,
}

#[derive(Deserialize, JsonSchema)]
pub struct AddTransactionParams {
    pub budget_id: Option<Uuid>,
    pub category_id: Option<Uuid>,
    pub amount: f64,
    pub description: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct ListTransactionsParams {
    pub budget_id: Option<Uuid>,
}

#[derive(Deserialize, JsonSchema)]
pub struct EditTransactionParams {
    pub budget_id: Option<Uuid>,
    pub transaction_id: Uuid,
    pub category_id: Option<Uuid>,
    pub amount: Option<f64>,
    pub description: Option<String>,
    pub excluded_from_budget: Option<bool>,
}

#[derive(Deserialize, JsonSchema)]
pub struct DeleteTransactionParams {
    pub budget_id: Option<Uuid>,
    pub transaction_id: Uuid,
}

#[derive(Deserialize, JsonSchema)]
pub struct CreateCategoryParams {
    pub budget_id: Option<Uuid>,
    pub name: String,
    pub category_type: String, // 'expense', 'income', 'savings'
    pub category_limit: Option<f64>,
    pub rollover_enabled: Option<bool>,
}

#[derive(Deserialize, JsonSchema)]
pub struct UpdateCategoryParams {
    pub budget_id: Option<Uuid>,
    pub category_id: Uuid,
    pub name: Option<String>,
    pub category_type: Option<String>,
    pub category_limit: Option<f64>,
    pub rollover_enabled: Option<bool>,
    pub is_fund: Option<bool>,
}

#[derive(Deserialize, JsonSchema)]
pub struct SetCategoryFundParams {
    pub budget_id: Option<Uuid>,
    pub category_id: Uuid,
    pub is_fund: bool,
}

#[derive(Deserialize, JsonSchema)]
pub struct SetRetirementProfileParams {
    pub country: Option<String>,
    pub birth_date: Option<String>,
    pub current_gross_income: Option<f64>,
    pub target_retirement_age: Option<i32>,
    pub contribution_rate_pre_tax: Option<f64>,
    pub contribution_rate_roth: Option<f64>,
    pub expected_real_return: Option<f64>,
    pub inflation_rate: Option<f64>,
    pub target_replacement_ratio: Option<f64>,
    pub life_expectancy_age: Option<i32>,
    pub ss_monthly_benefit: Option<f64>,
    pub ss_benefit_at_age: Option<i32>,
    pub ss_claiming_age: Option<i32>,
}

#[derive(Deserialize, JsonSchema)]
pub struct RetirementProjectionParams {
    pub target_retirement_age_override: Option<u32>,
    pub contribution_rate_pre_tax_override: Option<f64>,
    pub contribution_rate_roth_override: Option<f64>,
    pub expected_real_return_override: Option<f64>,
}

#[derive(Deserialize, JsonSchema)]
pub struct CreateGoalParams {
    pub budget_id: Option<Uuid>,
    pub name: String,
    pub goal_type: String, // 'savings', 'debt'
    pub target_amount: f64,
    pub target_date: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct CreateReminderParams {
    pub budget_id: Option<Uuid>,
    pub message: String,
    pub cadence: String, // 'daily', 'weekly', 'monthly'
}

#[tool_router]
impl NelsMcpServer {
    #[tool(description = "Create a new budget in Nels")]
    pub async fn nels_create_budget(
        &self,
        ctx: RequestContext<rmcp::RoleServer>,
        Parameters(params): Parameters<CreateBudgetParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let user_id = self.extract_user_id(&ctx).await?;
        self.create_budget_for_user(user_id, params).await
    }

    #[tool(description = "List budgets owned by or shared with the authenticated user")]
    pub async fn nels_list_budgets(
        &self,
        ctx: RequestContext<rmcp::RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let user_id = self.extract_user_id(&ctx).await?;
        self.list_budgets_for_user(user_id).await
    }

    #[tool(description = "Update an existing budget in Nels")]
    pub async fn nels_update_budget(
        &self,
        ctx: RequestContext<rmcp::RoleServer>,
        Parameters(params): Parameters<UpdateBudgetParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let user_id = self.extract_user_id(&ctx).await?;
        self.update_budget_for_user(user_id, params).await
    }

    #[tool(description = "Close a project budget in Nels")]
    pub async fn nels_close_budget(
        &self,
        ctx: RequestContext<rmcp::RoleServer>,
        Parameters(params): Parameters<BudgetIdParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let user_id = self.extract_user_id(&ctx).await?;
        self.close_budget_for_user(user_id, params).await
    }

    #[tool(description = "Archive a budget in Nels")]
    pub async fn nels_archive_budget(
        &self,
        ctx: RequestContext<rmcp::RoleServer>,
        Parameters(params): Parameters<BudgetIdParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let user_id = self.extract_user_id(&ctx).await?;
        self.archive_budget_for_user(user_id, params).await
    }

    #[tool(description = "Unarchive a budget in Nels")]
    pub async fn nels_unarchive_budget(
        &self,
        ctx: RequestContext<rmcp::RoleServer>,
        Parameters(params): Parameters<BudgetIdParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let user_id = self.extract_user_id(&ctx).await?;
        self.unarchive_budget_for_user(user_id, params).await
    }

    #[tool(description = "Roll up a child budget into a parent budget as a mirror category")]
    pub async fn nels_rollup_budget(
        &self,
        ctx: RequestContext<rmcp::RoleServer>,
        Parameters(params): Parameters<RollupParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let user_id = self.extract_user_id(&ctx).await?;
        self.rollup_budget_for_user(user_id, params).await
    }

    #[tool(description = "Unroll a child budget from its parent budget")]
    pub async fn nels_unrollup_budget(
        &self,
        ctx: RequestContext<rmcp::RoleServer>,
        Parameters(params): Parameters<RollupParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let user_id = self.extract_user_id(&ctx).await?;
        self.unrollup_budget_for_user(user_id, params).await
    }

    #[tool(description = "Add a new transaction to a budget")]
    pub async fn nels_add_transaction(
        &self,
        ctx: RequestContext<rmcp::RoleServer>,
        Parameters(params): Parameters<AddTransactionParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let user_id = self.extract_user_id(&ctx).await?;
        self.add_transaction_for_user(user_id, params).await
    }

    #[tool(description = "List recent transactions in a budget")]
    pub async fn nels_list_transactions(
        &self,
        ctx: RequestContext<rmcp::RoleServer>,
        Parameters(params): Parameters<ListTransactionsParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let user_id = self.extract_user_id(&ctx).await?;
        self.list_transactions_for_user(user_id, params).await
    }

    #[tool(description = "Edit an existing transaction in a budget")]
    pub async fn nels_edit_transaction(
        &self,
        ctx: RequestContext<rmcp::RoleServer>,
        Parameters(params): Parameters<EditTransactionParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let user_id = self.extract_user_id(&ctx).await?;
        self.edit_transaction_for_user(user_id, params).await
    }

    #[tool(description = "Delete a transaction from a budget")]
    pub async fn nels_delete_transaction(
        &self,
        ctx: RequestContext<rmcp::RoleServer>,
        Parameters(params): Parameters<DeleteTransactionParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let user_id = self.extract_user_id(&ctx).await?;
        self.delete_transaction_for_user(user_id, params).await
    }

    #[tool(description = "Create a new category in a budget")]
    pub async fn nels_create_category(
        &self,
        ctx: RequestContext<rmcp::RoleServer>,
        Parameters(params): Parameters<CreateCategoryParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let user_id = self.extract_user_id(&ctx).await?;
        self.create_category_for_user(user_id, params).await
    }

    #[tool(description = "Update an existing category in a budget")]
    pub async fn nels_update_category(
        &self,
        ctx: RequestContext<rmcp::RoleServer>,
        Parameters(params): Parameters<UpdateCategoryParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let user_id = self.extract_user_id(&ctx).await?;
        self.update_category_for_user(user_id, params).await
    }

    #[tool(description = "Enable or disable envelope fund status for a category")]
    pub async fn nels_set_category_fund(
        &self,
        ctx: RequestContext<rmcp::RoleServer>,
        Parameters(params): Parameters<SetCategoryFundParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let user_id = self.extract_user_id(&ctx).await?;
        self.set_category_fund_for_user(user_id, params).await
    }

    #[tool(description = "Set or update retirement profile and Social Security details")]
    pub async fn nels_set_retirement_profile(
        &self,
        ctx: RequestContext<rmcp::RoleServer>,
        Parameters(params): Parameters<SetRetirementProfileParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let user_id = self.extract_user_id(&ctx).await?;
        self.set_retirement_profile_for_user(user_id, params).await
    }

    #[tool(description = "Run a Monte Carlo retirement projection for the user")]
    pub async fn nels_run_retirement_projection(
        &self,
        ctx: RequestContext<rmcp::RoleServer>,
        Parameters(params): Parameters<RetirementProjectionParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let user_id = self.extract_user_id(&ctx).await?;
        self.run_retirement_projection_for_user(user_id, params).await
    }

    #[tool(description = "List linked investment and retirement assets")]
    pub async fn nels_list_assets(
        &self,
        ctx: RequestContext<rmcp::RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let user_id = self.extract_user_id(&ctx).await?;
        self.list_assets_for_user(user_id).await
    }

    #[tool(description = "Create a savings or debt payoff goal in a budget")]
    pub async fn nels_create_goal(
        &self,
        ctx: RequestContext<rmcp::RoleServer>,
        Parameters(params): Parameters<CreateGoalParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let user_id = self.extract_user_id(&ctx).await?;
        self.create_goal_for_user(user_id, params).await
    }

    #[tool(description = "Create a payment or budget reminder")]
    pub async fn nels_create_reminder(
        &self,
        ctx: RequestContext<rmcp::RoleServer>,
        Parameters(params): Parameters<CreateReminderParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let user_id = self.extract_user_id(&ctx).await?;
        self.create_reminder_for_user(user_id, params).await
    }
}

#[rmcp::tool_handler]
impl ServerHandler for NelsMcpServer {
    fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<rmcp::RoleServer>,
    ) -> impl std::future::Future<Output = Result<ListResourcesResult, ErrorData>> + Send + '_ {
        async move {
            let resources = vec![
                Resource::new("nels://budget/active", "Active Budget Details")
                    .with_description("Details of the active budget including categories, totals, and limits"),
                Resource::new("nels://categories", "Categories View")
                    .with_description("Categories, limits, fund balances, and spend"),
                Resource::new("nels://retirement/profile", "Retirement Profile")
                    .with_description("User retirement profile and Social Security assumptions"),
                Resource::new("nels://assets", "Retirement & Investment Assets")
                    .with_description("Linked investment assets and balances"),
            ];
            Ok(ListResourcesResult::with_all_items(resources))
        }
    }

    fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        context: RequestContext<rmcp::RoleServer>,
    ) -> impl std::future::Future<Output = Result<ReadResourceResponse, ErrorData>> + Send + '_ {
        async move {
            let user_id = self.extract_user_id(&context).await?;

            let uri = request.uri.as_str();
            let text = match uri {
                "nels://budget/active" => {
                    let target_bid = self.resolve_target_budget(user_id, None).await?;
                    let b = crate::budget::get_budget(State(self.state.clone()), Path(target_bid), Extension(user_id)).await
                        .map_err(|(_code, msg)| ErrorData::internal_error(msg, None))?;
                    serde_json::to_string_pretty(&b.0).unwrap_or_default()
                }
                "nels://categories" => {
                    let target_bid = self.resolve_target_budget(user_id, None).await?;
                    let v = categories_view(State(self.state.clone()), Path(target_bid), Extension(user_id)).await
                        .map_err(|(_code, msg)| ErrorData::internal_error(msg, None))?;
                    serde_json::to_string_pretty(&v.0).unwrap_or_default()
                }
                "nels://retirement/profile" => {
                    let prof = get_profile(&self.state.db, user_id).await
                        .map_err(|(_code, msg)| ErrorData::internal_error(msg, None))?;
                    serde_json::to_string_pretty(&prof).unwrap_or_default()
                }
                "nels://assets" => {
                    let assets = list_assets(State(self.state.clone()), Extension(user_id)).await
                        .map_err(|(_code, msg)| ErrorData::internal_error(msg, None))?;
                    serde_json::to_string_pretty(&assets.0).unwrap_or_default()
                }
                _ => return Err(ErrorData::invalid_params(format!("Unknown resource URI: {uri}"), None)),
            };

            let contents = vec![ResourceContents::text(text, request.uri.clone())];

            Ok(ReadResourceResponse::Complete(ReadResourceResult::new(contents)))
        }
    }
}

pub async fn mcp_handler(
    State(state): State<AppState>,
    req: Request<Body>,
) -> impl IntoResponse {
    let server = NelsMcpServer::new(state);
    let session_manager = Arc::new(LocalSessionManager::default());
    let config = StreamableHttpServerConfig::default();

    let service = StreamableHttpService::new(
        move || Ok(server.clone()),
        session_manager,
        config,
    );

    service.handle(req).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Router,
        middleware,
    };
    use tower::ServiceExt;
    use crate::auth::auth_middleware;

    #[tokio::test]
    async fn mcp_endpoint_unauthenticated_returns_401() {
        let db_url = std::env::var("DATABASE_URL")
            .unwrap_or_else(|_| "postgres://postgres:postgrespassword@127.0.0.1:6153/budget_rag".to_string());
        let pool = match sqlx::PgPool::connect(&db_url).await {
            Ok(p) => p,
            Err(_) => return, // Skip if test DB not running
        };

        let state = AppState {
            db: pool,
            cipher: Arc::new(crate::crypto::SecretCipher::new(&[0u8; 32]).unwrap()), webauthn: std::sync::Arc::new(crate::passkeys::WebauthnRegistry::for_test()),
        };

        let app = Router::new()
            .route("/api/mcp", axum::routing::any(mcp_handler))
            .layer(middleware::from_fn_with_state(state.clone(), auth_middleware))
            .with_state(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/mcp")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2026-07-28","capabilities":{},"clientInfo":{"name":"test","version":"1.0"}}}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), axum::http::StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn mcp_endpoint_authenticated_tools_and_resources() {
        let db_url = std::env::var("DATABASE_URL")
            .unwrap_or_else(|_| "postgres://postgres:postgrespassword@127.0.0.1:6153/budget_rag".to_string());
        let pool = match sqlx::PgPool::connect(&db_url).await {
            Ok(p) => p,
            Err(_) => return, // Skip if test DB not running
        };

        let state = AppState {
            db: pool.clone(),
            cipher: Arc::new(crate::crypto::SecretCipher::new(&[0u8; 32]).unwrap()), webauthn: std::sync::Arc::new(crate::passkeys::WebauthnRegistry::for_test()),
        };

        // Create test user and session
        let user_id = Uuid::new_v4();
        let email = format!("mcp_test_{user_id}@nels.money");
        let session_token = Uuid::new_v4().to_string();

        let _ = sqlx::query("INSERT INTO users (id, email) VALUES ($1, $2)")
            .bind(user_id)
            .bind(&email)
            .execute(&pool)
            .await;

        let _ = sqlx::query("INSERT INTO sessions (token, user_id, expires_at) VALUES ($1, $2, NOW() + INTERVAL '1 day')")
            .bind(&session_token)
            .bind(user_id)
            .execute(&pool)
            .await;

        let app = Router::new()
            .route("/api/mcp", axum::routing::any(mcp_handler))
            .layer(middleware::from_fn_with_state(state.clone(), auth_middleware))
            .with_state(state);

        // 1. Initialize
        let init_req = Request::builder()
            .method("POST")
            .uri("/api/mcp")
            .header("authorization", format!("Bearer {session_token}"))
            .header("content-type", "application/json")
            .body(Body::from(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2026-07-28","capabilities":{},"clientInfo":{"name":"test","version":"1.0"}}}"#))
            .unwrap();

        let res = app.clone().oneshot(init_req).await.unwrap();
        assert_eq!(res.status(), axum::http::StatusCode::OK);

        // 2. List Tools
        let tools_req = Request::builder()
            .method("POST")
            .uri("/api/mcp")
            .header("authorization", format!("Bearer {session_token}"))
            .header("content-type", "application/json")
            .body(Body::from(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#))
            .unwrap();

        let res = app.clone().oneshot(tools_req).await.unwrap();
        assert_eq!(res.status(), axum::http::StatusCode::OK);
        let body_bytes = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
        let body_str = String::from_utf8_lossy(&body_bytes);
        assert!(body_str.contains("nels_create_budget"));
        assert!(body_str.contains("nels_add_transaction"));
        assert!(body_str.contains("nels_run_retirement_projection"));

        // 3. List Resources
        let resources_req = Request::builder()
            .method("POST")
            .uri("/api/mcp")
            .header("authorization", format!("Bearer {session_token}"))
            .header("content-type", "application/json")
            .body(Body::from(r#"{"jsonrpc":"2.0","id":3,"method":"resources/list"}"#))
            .unwrap();

        let res = app.clone().oneshot(resources_req).await.unwrap();
        assert_eq!(res.status(), axum::http::StatusCode::OK);
        let body_bytes = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
        let body_str = String::from_utf8_lossy(&body_bytes);
        assert!(body_str.contains("nels://budget/active"));
        assert!(body_str.contains("nels://categories"));

        // Cleanup
        let _ = sqlx::query("DELETE FROM sessions WHERE token = $1").bind(&session_token).execute(&pool).await;
        let _ = sqlx::query("DELETE FROM users WHERE id = $1").bind(user_id).execute(&pool).await;
    }
}
