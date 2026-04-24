//! Verifies `format_traceback` produces the Display line exactly once when
//! the wrapper's Debug impl already starts with the Display line.

strider_error::define_error! {
    pub struct MyError wraps MyKind;

    #[derive(Debug, thiserror::Error)]
    pub enum MyKind {
        #[error("unique-display-marker-7a3f")]
        Boom,
    }
}

#[test]
fn format_traceback_prints_wrapper_display_exactly_once() {
    let err: MyError = MyKind::Boom.into();
    let s = strider_error::format_traceback(&err);
    let count = s.matches("unique-display-marker-7a3f").count();
    assert_eq!(
        count, 1,
        "expected the Display line once; got {count} occurrences in:\n{s}",
    );
    // Location markers must still be present.
    assert!(s.contains("  at [0] "), "locations dropped; got:\n{s}");
}

#[test]
fn format_traceback_walks_source_chain_top_to_bottom() {
    use std::fmt;

    #[derive(Debug)]
    struct Inner;
    impl fmt::Display for Inner {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str("inner-marker") }
    }
    impl std::error::Error for Inner {}

    #[derive(Debug)]
    struct Outer { inner: Inner }
    impl fmt::Display for Outer {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str("outer-marker") }
    }
    impl std::error::Error for Outer {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> { Some(&self.inner) }
    }

    let s = strider_error::format_traceback(&Outer { inner: Inner });
    let outer_at = s.find("outer-marker").expect("outer printed");
    let caused_at = s.find("caused by:").expect("caused-by line present");
    let inner_at = s.find("inner-marker").expect("inner printed");
    assert!(outer_at < caused_at && caused_at < inner_at, "ordering wrong in:\n{s}");
}
