//! `OpenAPI` schemas for the types the API carries, with the same bounds the
//! types enforce.

use utoipa::openapi::schema::{ArrayBuilder, KnownFormat, ObjectBuilder, SchemaFormat, Type};
use utoipa::openapi::{RefOr, Schema};
use utoipa::{PartialSchema, ToSchema};

use crate::text::{
    Body, Id, Label, Labels, MAX_BODY, MAX_LABEL, MAX_LABELS, MAX_NAME, MAX_TITLE, Name, Title,
};
use crate::time::At;

macro_rules! string_schema {
    ($ty:ty, min: $min:expr, max: $max:expr, pattern: $pattern:expr, example: $example:literal, $description:literal) => {
        impl PartialSchema for $ty {
            fn schema() -> RefOr<Schema> {
                ObjectBuilder::new()
                    .schema_type(Type::String)
                    .description(Some($description))
                    .min_length($min)
                    .max_length(Some($max))
                    .pattern($pattern)
                    .examples([$example])
                    .build()
                    .into()
            }
        }

        impl ToSchema for $ty {}
    };
}

string_schema!(
    Name,
    min: Some(1),
    max: MAX_NAME,
    pattern: Some("^[a-z0-9](-?[a-z0-9])*$"),
    example: "alice",
    "A participant's name: lower-case letters and digits with single inner hyphens."
);

string_schema!(
    Id,
    min: Some(36),
    max: 36,
    pattern: Some("^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$"),
    example: "765a0b0c-9a3e-4f1d-8c2b-1e2f3a4b5c6d",
    "A lower-case UUID."
);

string_schema!(
    Title,
    min: Some(1),
    max: MAX_TITLE,
    pattern: Some(r"^[^\x00-\x1F\x7F-\x9F\u200E\u200F\u2028-\u202E\u2066-\u2069]+$"),
    example: "Bump the reth image",
    "One trimmed line of at most 200 characters, without control, line or paragraph separator, or bidirectional characters."
);

string_schema!(
    Body,
    min: None,
    max: MAX_BODY,
    pattern: Some(r"^[^\x00-\x08\x0B\x0C\x0E-\x1F\x7F-\x9F\u200E\u200F\u2028-\u202E\u2066-\u2069]*$"),
    example: "CI is red on main since the bump.",
    "Text of at most 65536 characters, without control characters other than tab and line breaks, or bidirectional or separator characters. Markdown is conventional."
);

string_schema!(
    Label,
    min: Some(1),
    max: MAX_LABEL,
    pattern: Some("^[a-z0-9][a-z0-9._/-]*$"),
    example: "infra",
    "A thread label."
);

impl PartialSchema for Labels {
    fn schema() -> RefOr<Schema> {
        ArrayBuilder::new()
            .items(Label::schema())
            .max_items(Some(MAX_LABELS))
            .unique_items(true)
            .description(Some("Labels, sorted and unique."))
            .build()
            .into()
    }
}

impl ToSchema for Labels {}

impl PartialSchema for At {
    fn schema() -> RefOr<Schema> {
        ObjectBuilder::new()
            .schema_type(Type::String)
            .format(Some(SchemaFormat::KnownFormat(KnownFormat::DateTime)))
            .pattern(Some(r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z$"))
            .min_length(Some(20))
            .max_length(Some(20))
            .description(Some("A UTC time to the second, as YYYY-MM-DDTHH:MM:SSZ."))
            .examples(["2026-10-07T12:00:00Z"])
            .build()
            .into()
    }
}

impl ToSchema for At {}
