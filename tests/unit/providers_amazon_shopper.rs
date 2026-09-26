use super::*;

#[test]
fn place_needs_checkout_from_an_earlier_turn() {
    let sessions = ShopperSessions::new();
    let user = Uuid::new_v4();
    let (checkout_turn, reply_turn) = (Uuid::new_v4(), Uuid::new_v4());

    assert_eq!(
        sessions.authorize_place(user, checkout_turn),
        PlaceDecision::NoCheckout
    );

    sessions.record_checkout(user, checkout_turn);
    assert_eq!(
        sessions.authorize_place(user, checkout_turn),
        PlaceDecision::SameTurn
    );
    // A same-turn attempt leaves the checkout ready for the caller's answer.
    assert_eq!(
        sessions.authorize_place(user, reply_turn),
        PlaceDecision::Allowed
    );
    // Consumed: placing again needs a fresh checkout.
    assert_eq!(
        sessions.authorize_place(user, Uuid::new_v4()),
        PlaceDecision::NoCheckout
    );
}

#[test]
fn browsing_after_checkout_invalidates_it() {
    let sessions = ShopperSessions::new();
    let user = Uuid::new_v4();
    sessions.record_checkout(user, Uuid::new_v4());
    sessions.touch(user);
    assert_eq!(
        sessions.authorize_place(user, Uuid::new_v4()),
        PlaceDecision::NoCheckout
    );
    assert!(sessions.is_active(user));
}

#[test]
fn sessions_are_per_user() {
    let sessions = ShopperSessions::new();
    let (alice, bob) = (Uuid::new_v4(), Uuid::new_v4());
    sessions.record_checkout(alice, Uuid::new_v4());
    assert!(!sessions.is_active(bob));
    assert_eq!(
        sessions.authorize_place(bob, Uuid::new_v4()),
        PlaceDecision::NoCheckout
    );
}

#[test]
fn stale_sessions_expire() {
    let sessions = ShopperSessions::with_ttl(Duration::ZERO);
    let user = Uuid::new_v4();
    sessions.record_checkout(user, Uuid::new_v4());
    assert!(!sessions.is_active(user));
    assert_eq!(
        sessions.authorize_place(user, Uuid::new_v4()),
        PlaceDecision::NoCheckout
    );
}

#[test]
fn ended_session_is_inactive() {
    let sessions = ShopperSessions::new();
    let user = Uuid::new_v4();
    sessions.touch(user);
    sessions.end(user);
    assert!(!sessions.is_active(user));
}

#[test]
fn reads_helper_status() {
    assert_eq!(
        response_status(&json!({"status": "cod_unavailable", "total": "₹79,900"})),
        "cod_unavailable"
    );
    assert_eq!(response_status(&json!({"results": []})), "");
}
