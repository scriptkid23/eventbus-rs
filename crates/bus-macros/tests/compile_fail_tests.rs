#[test]
fn compile_fail_cases() {
    let test_cases = trybuild::TestCases::new();

    test_cases.compile_fail("tests/compile_fail/missing_id.rs");
    test_cases.compile_fail("tests/compile_fail/missing_interpolation_field.rs");
    test_cases.compile_fail("tests/compile_fail/missing_subject.rs");
    test_cases.compile_fail("tests/compile_fail/wrong_id_type.rs");
}
