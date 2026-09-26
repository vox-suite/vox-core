use super::state_note;
use serde_json::json;

#[test]
fn ready_checkout_tells_the_agent_to_continue_not_restart() {
    let note = state_note(
        "amazon_checkout",
        &json!({"status": "ok", "total": "₹250.00", "delivery": "Arriving by 28 Sept"}),
    )
    .expect("note");
    assert!(note.contains("₹250.00"));
    assert!(note.contains("amazon_place_order"));
    assert!(note.contains("Do not search again"));
}

#[test]
fn loading_or_prepared_checkout_points_back_to_checkout() {
    for (tool, status) in [
        ("amazon_checkout", "in_progress"),
        ("amazon_search", "checkout_ready"),
    ] {
        let note = state_note(tool, &json!({"status": status})).expect("note");
        assert!(note.contains("amazon_checkout (not amazon_search)"));
    }
}

#[test]
fn open_product_note_names_the_product() {
    let note = state_note(
        "amazon_open_product",
        &json!({"status": "ok", "title": "Duracell AA, Pack of 4", "price": "₹203.00"}),
    )
    .expect("note");
    assert!(note.contains("Duracell AA, Pack of 4 at ₹203.00"));
}

#[test]
fn plain_search_results_leave_no_note() {
    assert_eq!(state_note("amazon_search", &json!({"status": "ok"})), None);
}
