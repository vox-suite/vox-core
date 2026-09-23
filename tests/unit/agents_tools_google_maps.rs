use super::*;

#[test]
fn waypoint_supports_address_place_id_and_coordinates() {
    assert_eq!(
        route_waypoint("Pune Airport"),
        serde_json::json!({ "address": "Pune Airport" })
    );
    assert_eq!(
        route_waypoint("place_id:ChIJExample"),
        serde_json::json!({ "placeId": "ChIJExample" })
    );
    assert_eq!(
        route_waypoint("18.5204,73.8567"),
        serde_json::json!({
            "location": {
                "latLng": {
                    "latitude": 18.5204,
                    "longitude": 73.8567
                }
            }
        })
    );
}

#[test]
fn places_response_is_reduced_for_the_agent() {
    let response = serde_json::json!({
        "places": [{
            "id": "ChIJExample",
            "displayName": { "text": "Example Cafe", "languageCode": "en" },
            "formattedAddress": "Pune, Maharashtra",
            "location": { "latitude": 18.5204, "longitude": 73.8567 },
            "rating": 4.6,
            "currentOpeningHours": { "openNow": true },
            "reviews": [{ "text": { "text": "This must not be returned" } }]
        }]
    });

    let reduced = reduce_places_response(response);

    assert_eq!(reduced["places"][0]["id"], "ChIJExample");
    assert_eq!(reduced["places"][0]["name"], "Example Cafe");
    assert_eq!(reduced["places"][0]["open_now"], true);
    assert!(reduced["places"][0].get("reviews").is_none());
}
