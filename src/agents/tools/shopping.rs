/**
 * Agent tools that shop on amazon.in through the local Playwright helper:
 * search, open a product, pick its variants, prepare checkout (default
 * address, Pay on Delivery), and place the order once the caller confirms.
 */
use crate::{
    db::Db,
    identity::UserId,
    providers::{PlaceDecision, ShopperClient, ShopperError, ShopperSessions, response_status},
};
use rig::tool::{Tool, ToolContext};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use uuid::Uuid;

/// What every shopping tool in one agent turn shares.
#[derive(Clone)]
pub struct ShoppingContext {
    client: ShopperClient,
    sessions: ShopperSessions,
    db: Option<Db>,
    user_id: UserId,
    /// Identifies the agent turn; an order is only placed from a turn after
    /// the one that prepared checkout (see `ShopperSessions`).
    turn: Uuid,
}

impl ShoppingContext {
    pub fn new(
        client: ShopperClient,
        sessions: ShopperSessions,
        db: Option<Db>,
        user_id: UserId,
        turn: Uuid,
    ) -> Self {
        Self {
            client,
            sessions,
            db,
            user_id,
            turn,
        }
    }
}

/// Transport failures become a status the agent can say out loud instead of
/// an error that ends the turn.
fn outcome(tool: &str, result: Result<Value, ShopperError>) -> Value {
    match result {
        Ok(value) => {
            tracing::info!(tool, status = response_status(&value), "Tool completed");
            value
        }
        Err(err) => {
            tracing::error!(tool, error = %err, "Tool failed");
            json!({ "status": "helper_unavailable", "message": err.to_string() })
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct AmazonSearchArgs {
    pub query: String,
}

#[derive(Clone)]
pub struct AmazonSearch(ShoppingContext);

impl AmazonSearch {
    pub fn new(ctx: ShoppingContext) -> Self {
        Self(ctx)
    }
}

impl Tool for AmazonSearch {
    const NAME: &'static str = "amazon_search";
    type Args = AmazonSearchArgs;
    type Output = Value;
    type Error = ShopperError;

    fn description(&self) -> String {
        "Search Amazon India (amazon.in) in the user's browser for a product they want to buy. \
         Returns the top results with ASIN, title, price and rating. In the same turn, call \
         amazon_open_product with the result that best matches what the user asked for."
            .into()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "What to search for, e.g. 'iPhone 16'" }
            },
            "required": ["query"]
        })
    }

    async fn call(
        &self,
        _context: &mut ToolContext,
        args: Self::Args,
    ) -> Result<Value, ShopperError> {
        tracing::info!(tool = Self::NAME, query = %args.query, "Tool called");
        self.0.sessions.touch(self.0.user_id.0);
        Ok(outcome(
            Self::NAME,
            self.0.client.search(args.query.trim()).await,
        ))
    }
}

#[derive(Debug, Deserialize)]
pub struct AmazonOpenProductArgs {
    pub asin: String,
}

#[derive(Clone)]
pub struct AmazonOpenProduct(ShoppingContext);

impl AmazonOpenProduct {
    pub fn new(ctx: ShoppingContext) -> Self {
        Self(ctx)
    }
}

impl Tool for AmazonOpenProduct {
    const NAME: &'static str = "amazon_open_product";
    type Args = AmazonOpenProductArgs;
    type Output = Value;
    type Error = ShopperError;

    fn description(&self) -> String {
        "Open an Amazon product page by ASIN. Returns the title, price, availability and the variant \
         choices (such as Colour and Size or storage) with the currently selected option. Ask the \
         user to choose every variant dimension that has more than one available option."
            .into()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "asin": { "type": "string", "description": "The product's ASIN from amazon_search" }
            },
            "required": ["asin"]
        })
    }

    async fn call(
        &self,
        _context: &mut ToolContext,
        args: Self::Args,
    ) -> Result<Value, ShopperError> {
        tracing::info!(tool = Self::NAME, asin = %args.asin, "Tool called");
        self.0.sessions.touch(self.0.user_id.0);
        Ok(outcome(
            Self::NAME,
            self.0.client.open_product(args.asin.trim()).await,
        ))
    }
}

#[derive(Debug, Deserialize)]
pub struct AmazonSelectOptionsArgs {
    pub options: HashMap<String, String>,
}

#[derive(Clone)]
pub struct AmazonSelectOptions(ShoppingContext);

impl AmazonSelectOptions {
    pub fn new(ctx: ShoppingContext) -> Self {
        Self(ctx)
    }
}

impl Tool for AmazonSelectOptions {
    const NAME: &'static str = "amazon_select_options";
    type Args = AmazonSelectOptionsArgs;
    type Output = Value;
    type Error = ShopperError;

    fn description(&self) -> String {
        "Select variant options on the open Amazon product page, using the dimension names and \
         option labels exactly as amazon_open_product returned them. Returns the updated title, \
         price and selections."
            .into()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "options": {
                    "type": "object",
                    "description": "Dimension name to option label, e.g. {\"Colour\": \"Teal\", \"Size\": \"256 GB\"}",
                    "additionalProperties": { "type": "string" }
                }
            },
            "required": ["options"]
        })
    }

    async fn call(
        &self,
        _context: &mut ToolContext,
        args: Self::Args,
    ) -> Result<Value, ShopperError> {
        tracing::info!(tool = Self::NAME, options = ?args.options, "Tool called");
        self.0.sessions.touch(self.0.user_id.0);
        Ok(outcome(
            Self::NAME,
            self.0.client.select_options(&args.options).await,
        ))
    }
}

#[derive(Debug, Deserialize)]
pub struct AmazonCheckoutArgs {
    pub quantity: Option<u32>,
}

#[derive(Clone)]
pub struct AmazonCheckout(ShoppingContext);

impl AmazonCheckout {
    pub fn new(ctx: ShoppingContext) -> Self {
        Self(ctx)
    }
}

impl Tool for AmazonCheckout {
    const NAME: &'static str = "amazon_checkout";
    type Args = AmazonCheckoutArgs;
    type Output = Value;
    type Error = ShopperError;

    fn description(&self) -> String {
        "Start checkout for the open Amazon product with Buy Now, using the account's default \
         delivery address and Cash/Pay on Delivery. Returns the order total, delivery address and \
         delivery estimate. This does NOT place the order: read the total to the user and ask them \
         to confirm."
            .into()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "quantity": { "type": "integer", "minimum": 1, "maximum": 10, "description": "Defaults to 1" }
            }
        })
    }

    async fn call(
        &self,
        _context: &mut ToolContext,
        args: Self::Args,
    ) -> Result<Value, ShopperError> {
        let quantity = args.quantity.unwrap_or(1).clamp(1, 10);
        tracing::info!(tool = Self::NAME, quantity, "Tool called");
        let user_id = self.0.user_id.0;
        self.0.sessions.touch(user_id);
        let result = outcome(Self::NAME, self.0.client.checkout(quantity).await);
        if response_status(&result) == "ok" {
            self.0.sessions.record_checkout(user_id, self.0.turn);
        }
        Ok(result)
    }
}

#[derive(Debug, Deserialize)]
pub struct AmazonPlaceOrderArgs {}

#[derive(Clone)]
pub struct AmazonPlaceOrder(ShoppingContext);

impl AmazonPlaceOrder {
    pub fn new(ctx: ShoppingContext) -> Self {
        Self(ctx)
    }
}

async fn audit_order(db: &Db, user_id: Uuid, details: Value) {
    let result = sqlx::query(
        "INSERT INTO audit_events (user_id, actor, event_type, affected_ids, details) \
         VALUES ($1, 'agent', 'amazon.order', '[]'::jsonb, $2)",
    )
    .bind(user_id)
    .bind(details)
    .execute(db.pool())
    .await;
    if let Err(err) = result {
        tracing::error!(%err, "failed to audit amazon order");
    }
}

impl Tool for AmazonPlaceOrder {
    const NAME: &'static str = "amazon_place_order";
    type Args = AmazonPlaceOrderArgs;
    type Output = Value;
    type Error = ShopperError;

    fn description(&self) -> String {
        "Place the Amazon order prepared by amazon_checkout. Only call this after the user has \
         heard the total and explicitly said yes in their latest message. The order is placed only \
         when this returns status 'placed'."
            .into()
    }

    fn parameters(&self) -> Value {
        json!({ "type": "object", "properties": {} })
    }

    async fn call(
        &self,
        _context: &mut ToolContext,
        _args: Self::Args,
    ) -> Result<Value, ShopperError> {
        tracing::info!(tool = Self::NAME, "Tool called");
        let user_id = self.0.user_id.0;
        match self.0.sessions.authorize_place(user_id, self.0.turn) {
            PlaceDecision::Allowed => {}
            PlaceDecision::SameTurn => {
                return Ok(json!({
                    "status": "awaiting_user",
                    "message": "The user has not confirmed yet. Read them the total and ask; call amazon_place_order only after they say yes."
                }));
            }
            PlaceDecision::NoCheckout => {
                return Ok(json!({
                    "status": "no_checkout",
                    "message": "No checkout is ready. Call amazon_checkout first and read the total to the user."
                }));
            }
        }

        let result = outcome(Self::NAME, self.0.client.place_order().await);
        if matches!(response_status(&result), "placed" | "dry_run") {
            self.0.sessions.end(user_id);
        }
        if let Some(db) = &self.0.db {
            audit_order(db, user_id, result.clone()).await;
        }
        Ok(result)
    }
}
