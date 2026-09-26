/**
* Agent tools interfacing with Google Maps for place search and routing.
*/
use reqwest::Client;
use rig::tool::Tool;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{error::Error as StdError, fmt};

const PLACES_TEXT_SEARCH_URL: &str = "https://places.googleapis.com/v1/places:searchText";
const COMPUTE_ROUTES_URL: &str = "https://routes.googleapis.com/directions/v2:computeRoutes";

#[derive(Debug)]
pub enum GoogleMapsError {
    MissingApiKey,
    Request(reqwest::Error),
}

impl fmt::Display for GoogleMapsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingApiKey => formatter.write_str("GOOGLE_MAPS_API_KEY is not configured"),
            Self::Request(error) => error.fmt(formatter),
        }
    }
}

impl StdError for GoogleMapsError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Self::MissingApiKey => None,
            Self::Request(error) => Some(error),
        }
    }
}

impl From<reqwest::Error> for GoogleMapsError {
    fn from(error: reqwest::Error) -> Self {
        Self::Request(error)
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct SearchPlacesArgs {
    pub query: String,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub radius_meters: Option<f64>,
}

#[derive(Clone)]
pub struct SearchPlaces {
    client: Client,
    api_key: Option<String>,
}

impl SearchPlaces {
    pub fn new(client: Client, api_key: Option<String>) -> Self {
        Self { client, api_key }
    }
}

impl Tool for SearchPlaces {
    const NAME: &'static str = "search_places";
    type Args = SearchPlacesArgs;
    type Output = String;
    type Error = GoogleMapsError;

    fn description(&self) -> String {
        "Find real-world places by name, category, or description. Optionally bias results around a latitude and longitude."
            .to_owned()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Place or category to find, such as coffee near Pune"
                },
                "latitude": {
                    "type": "number",
                    "description": "Optional current latitude used with longitude"
                },
                "longitude": {
                    "type": "number",
                    "description": "Optional current longitude used with latitude"
                },
                "radius_meters": {
                    "type": "number",
                    "description": "Optional search radius from 100 to 50000 metres"
                }
            },
            "required": ["query"]
        })
    }

    async fn call(
        &self,
        _context: &mut rig::prelude::ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        let api_key = self
            .api_key
            .as_deref()
            .ok_or(GoogleMapsError::MissingApiKey)?;
        let started_at = std::time::Instant::now();
        tracing::info!(tool = Self::NAME, query = %args.query, "Tool called");
        let mut body = json!({ "textQuery": args.query });

        if let (Some(latitude), Some(longitude)) = (args.latitude, args.longitude) {
            body["locationBias"] = json!({
                "circle": {
                    "center": { "latitude": latitude, "longitude": longitude },
                    "radius": args.radius_meters.unwrap_or(5_000.0).clamp(100.0, 50_000.0)
                }
            });
        }

        let response = self
            .client
            .post(PLACES_TEXT_SEARCH_URL)
            .header("X-Goog-Api-Key", api_key)
            .header(
                "X-Goog-FieldMask",
                "places.id,places.displayName,places.formattedAddress,places.location,places.rating,places.currentOpeningHours.openNow",
            )
            .json(&body)
            .send()
            .await?;

        eprintln!(
            "agent_tool event=response tool=search_places status={} elapsed_ms={}",
            response.status(),
            started_at.elapsed().as_millis()
        );

        let response = response.error_for_status()?.json::<Value>().await?;
        let output = serde_json::to_string(&reduce_places_response(response))
            .expect("serializing JSON values cannot fail");

        tracing::info!(
            tool = Self::NAME,
            elapsed_ms = started_at.elapsed().as_millis(),
            response_bytes = output.len(),
            "Tool completed successfully"
        );

        Ok(output)
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct GetRouteArgs {
    pub origin: String,
    pub destination: String,
    pub travel_mode: Option<String>,
}

#[derive(Clone)]
pub struct GetRoute {
    client: Client,
    api_key: Option<String>,
}

impl GetRoute {
    pub fn new(client: Client, api_key: Option<String>) -> Self {
        Self { client, api_key }
    }
}

impl Tool for GetRoute {
    const NAME: &'static str = "get_route";
    type Args = GetRouteArgs;
    type Output = String;
    type Error = GoogleMapsError;

    fn description(&self) -> String {
        "Calculate distance and travel time between two addresses, Google Place IDs, or latitude/longitude coordinates. Prefix Place IDs with place_id:."
            .to_owned()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "origin": {
                    "type": "string",
                    "description": "Origin address, place_id:PLACE_ID, or latitude,longitude"
                },
                "destination": {
                    "type": "string",
                    "description": "Destination address, place_id:PLACE_ID, or latitude,longitude"
                },
                "travel_mode": {
                    "type": "string",
                    "enum": ["DRIVE", "WALK", "BICYCLE", "TWO_WHEELER", "TRANSIT"],
                    "description": "Travel mode; defaults to DRIVE"
                }
            },
            "required": ["origin", "destination"]
        })
    }

    async fn call(
        &self,
        _context: &mut rig::prelude::ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        let api_key = self
            .api_key
            .as_deref()
            .ok_or(GoogleMapsError::MissingApiKey)?;
        let started_at = std::time::Instant::now();
        let travel_mode = normalized_travel_mode(args.travel_mode.as_deref());
        let body = json!({
            "origin": route_waypoint(&args.origin),
            "destination": route_waypoint(&args.destination),
            "travelMode": travel_mode,
            "languageCode": "en",
            "units": "METRIC"
        });

        tracing::info!(
            tool = Self::NAME,
            origin = %args.origin,
            destination = %args.destination,
            travel_mode,
            "Tool called"
        );

        let response = self
            .client
            .post(COMPUTE_ROUTES_URL)
            .header("X-Goog-Api-Key", api_key)
            .header("X-Goog-FieldMask", "routes.distanceMeters,routes.duration")
            .json(&body)
            .send()
            .await?;

        let response = response.error_for_status()?.json::<Value>().await?;
        let output = serde_json::to_string(&reduce_routes_response(response))
            .expect("serializing JSON values cannot fail");

        tracing::info!(
            tool = Self::NAME,
            elapsed_ms = started_at.elapsed().as_millis(),
            response_bytes = output.len(),
            "Tool completed successfully"
        );

        Ok(output)
    }
}

fn normalized_travel_mode(mode: Option<&str>) -> &'static str {
    match mode.map(str::to_ascii_uppercase).as_deref() {
        Some("WALK") => "WALK",
        Some("BICYCLE") => "BICYCLE",
        Some("TWO_WHEELER") => "TWO_WHEELER",
        Some("TRANSIT") => "TRANSIT",
        _ => "DRIVE",
    }
}

fn route_waypoint(input: &str) -> Value {
    if let Some(place_id) = input.strip_prefix("place_id:") {
        return json!({ "placeId": place_id.trim() });
    }

    let coordinates = input.split_once(',').and_then(|(latitude, longitude)| {
        Some((
            latitude.trim().parse::<f64>().ok()?,
            longitude.trim().parse::<f64>().ok()?,
        ))
    });

    match coordinates {
        Some((latitude, longitude))
            if (-90.0..=90.0).contains(&latitude) && (-180.0..=180.0).contains(&longitude) =>
        {
            json!({
                "location": {
                    "latLng": { "latitude": latitude, "longitude": longitude }
                }
            })
        }
        _ => json!({ "address": input }),
    }
}

fn reduce_places_response(response: Value) -> Value {
    let places = response
        .get("places")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .take(5)
        .map(|place| {
            json!({
                "id": place.get("id"),
                "name": place.pointer("/displayName/text"),
                "address": place.get("formattedAddress"),
                "location": place.get("location"),
                "rating": place.get("rating"),
                "open_now": place.pointer("/currentOpeningHours/openNow")
            })
        })
        .collect::<Vec<_>>();

    json!({ "places": places })
}

fn reduce_routes_response(response: Value) -> Value {
    let routes = response
        .get("routes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .take(3)
        .map(|route| {
            json!({
                "distance_meters": route.get("distanceMeters"),
                "duration": route.get("duration")
            })
        })
        .collect::<Vec<_>>();

    json!({ "routes": routes })
}
