#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::uninlined_format_args
)]

use dbmr_schema::{parse_schema, print_schema};
use proptest::prelude::*;

proptest! {
    #[test]
    fn test_arbitrary_table_names_roundtrip(name in "[a-z][a-z0-9_]{1,10}") {
        let json = format!(r#"{{
            "adapter": "sqlite3",
            "framework": "express",
            "tables": {{
                "{}": {{
                    "columns": {{
                        "title": "required|string",
                        "count": "integer"
                    }}
                }}
            }}
        }}"#, name);

        let parsed1 = parse_schema(&json).unwrap();
        let printed1 = print_schema(&parsed1);
        let parsed2 = parse_schema(&printed1).unwrap();
        let printed2 = print_schema(&parsed2);

        prop_assert_eq!(printed1, printed2);
    }
}
