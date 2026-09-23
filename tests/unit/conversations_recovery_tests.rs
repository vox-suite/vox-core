use super::*;

#[test]
fn phone_fragments_accumulate_but_full_retries_replace() {
    assert_eq!(accumulate_phone("98765", "43210"), "9876543210");
    assert_eq!(accumulate_phone("123", "9876543210"), "9876543210");
    assert!(explicit_name("What tasks are due?").is_none());
    assert!(explicit_name("98765").is_none());
    assert_eq!(explicit_name("My name is Rahul"), Some("Rahul".into()));
}
