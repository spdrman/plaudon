//! Hostile and boundary CLI output, parsed under a version lock (spec 7.2, 18.3).
//!
//! Everything the vendor tool prints is untrusted text. These tests feed the
//! parser the shapes a real tool produces when it goes wrong — empty output,
//! colour escapes, partial JSON, duplicate keys, wrong types, locale-formatted
//! numbers, over-long lines, disagreeing row counts, impossible dates — and
//! assert the error, never a panic and never a half-read listing.

use std::fmt::Write as _;

use plaudon_domain::capability::Tri;
use plaudon_domain::source::{SourceAccountScope, SourceKind};
use plaudon_recording_sources::source::SourceError;
use plaudon_source_plaud_cli::parser::{MAX_RECORDS_PER_PAGE, MAX_TABLE_LINE_BYTES};
use plaudon_source_plaud_cli::{
    CliCompatibilityRow, CliEvidenceLevel, CliVersion, ListingCompleteness, ListingFormat,
    PaginationEvidence, ParsedListingPage, ParserError, VersionedParser,
};

// ----------------------------------------------------------------- fixtures

fn parser(format: ListingFormat, pagination: bool) -> VersionedParser {
    VersionedParser::candidate(
        CliVersion::new("1.4.2"),
        format,
        SourceAccountScope::new("account-a"),
        pagination,
    )
}

fn plaud_parser() -> VersionedParser {
    VersionedParser::candidate(
        CliVersion::new("0.3.15"),
        ListingFormat::PlaudTable03,
        SourceAccountScope::new("account-a"),
        true,
    )
}

fn external_ids(page: &ParsedListingPage) -> Vec<String> {
    page.items
        .iter()
        .map(|item| {
            item.identity
                .external_id
                .as_ref()
                .expect("external id")
                .to_string()
        })
        .collect()
}

/// One row in the geometry `@plaud-ai/cli` 0.3.15 prints.
fn plaud_row_text(id: &str, name: &str, date: &str, duration: &str) -> String {
    format!("  {id:<35}  {name:<36}  {date:<12}  {duration}")
}

fn plaud_page(rows: &[String], page: u32, declared: Option<usize>) -> String {
    let count = declared.unwrap_or(rows.len());
    let header = format!("  {:<36}{:<38}{:<14}DURATION", "ID", "NAME", "DATE");
    let rule = format!("  {}", "\u{2500}".repeat(98));
    let mut out = format!("\nFiles on this page: {count}\n\n{header}\n{rule}\n");
    for row in rows {
        out.push_str(row);
        out.push('\n');
    }
    let _ = write!(out, "\nPage {page}\n");
    out
}

fn one_plaud_row() -> String {
    plaud_row_text(
        "of_1a2b3c4d5e6f708192a3b4c5d6e7f801",
        "Board sync",
        "2026-10-12",
        "41m07s",
    )
}

fn compatibility_row(format: ListingFormat, pagination: PaginationEvidence) -> CliCompatibilityRow {
    CliCompatibilityRow {
        version: "0.3.15".to_owned(),
        listing_format: format,
        pagination,
        vendor_artifacts: Tri::Unknown,
        logout_command: Tri::Unknown,
        machine_readable: Tri::Unknown,
        evidence_level: CliEvidenceLevel::ObservedOnIdentifiedRelease,
        fixture_id: "plaud-cli-0.3.15".to_owned(),
        fixture_sha256: "0".repeat(64),
        verified_on: Some("2026-10-08".to_owned()),
        host_os: vec!["macos-15".to_owned()],
    }
}

// ------------------------------------------------------------ version strings

#[test]
fn a_version_is_read_only_from_an_exact_dotted_release_string() {
    for (output, expected) in [
        (
            "plaud 0.3.15\ncommit  0fa20ec\nbuilt   2026-10-08\n",
            "0.3.15",
        ),
        ("plaud/1.2.3\n", "1.2.3"),
        ("v1.2.3", "1.2.3"),
        ("1.2.3.4", "1.2.3.4"),
        ("0.0", "0.0"),
        ("\n\n   plaud 10.20.30   \n", "10.20.30"),
    ] {
        assert_eq!(
            CliVersion::parse_version_output(output)
                .as_ref()
                .map(CliVersion::as_str),
            Some(expected),
            "{output:?}"
        );
    }
}

#[test]
fn output_that_is_not_a_release_string_is_never_guessed_into_one() {
    for output in [
        "",
        "   ",
        "\n\t\n",
        "1",
        "1.",
        ".1",
        "1.2.3.4.5",
        "1.2.beta",
        "1..2",
        "plaud/",
        "v",
        "...",
        "1.2.3-beta.1",
        "nightly-2026-10-11",
        "plaud 1.2.3 (build 9)",
        "\u{1b}[1m1.2.3\u{1b}[0m",
    ] {
        assert!(
            CliVersion::parse_version_output(output).is_none(),
            "{output:?} must not be read as a version"
        );
    }
}

#[test]
fn a_parser_reports_the_version_and_format_it_was_pinned_to() {
    let parser = parser(ListingFormat::JsonLines, false);
    assert_eq!(parser.version().as_str(), "1.4.2");
    assert_eq!(parser.format(), ListingFormat::JsonLines);

    assert!(ListingFormat::JsonArray.is_machine_readable());
    assert!(ListingFormat::JsonLines.is_machine_readable());
    assert!(!ListingFormat::DelimitedTable.is_machine_readable());
    assert!(
        !ListingFormat::PlaudTable03.is_machine_readable(),
        "a scraped table is never a documented machine-readable format"
    );
}

#[test]
fn a_parser_built_from_a_row_inherits_that_row_s_paging_evidence() {
    let observed = VersionedParser::for_row(
        &compatibility_row(
            ListingFormat::JsonArray,
            PaginationEvidence::Observed {
                cursor_flag: "--cursor".to_owned(),
                page_size: 50,
            },
        ),
        SourceAccountScope::new("account-a"),
    );
    let page = observed
        .parse_listing(r#"{"files":[{"id":"rec_1"}],"next_cursor":"next"}"#)
        .expect("parsed");
    assert_eq!(page.next_cursor.as_deref(), Some("next"));
    assert_eq!(page.completeness, ListingCompleteness::Complete);
    assert_eq!(observed.version().as_str(), "0.3.15");

    let numbered = VersionedParser::for_row(
        &compatibility_row(
            ListingFormat::JsonArray,
            PaginationEvidence::PageNumber {
                page_flag: "--page".to_owned(),
                page_size_flag: "--limit".to_owned(),
                page_size: 20,
            },
        ),
        SourceAccountScope::new("account-a"),
    );
    assert_eq!(
        numbered
            .parse_listing(r#"{"files":[{"id":"rec_1"}],"next_cursor":"next"}"#)
            .expect("parsed")
            .next_cursor
            .as_deref(),
        Some("next")
    );

    let unobserved = VersionedParser::for_row(
        &compatibility_row(ListingFormat::JsonArray, PaginationEvidence::NotObserved),
        SourceAccountScope::new("account-a"),
    );
    let page = unobserved
        .parse_listing(r#"{"files":[{"id":"rec_1"}],"next_cursor":"next"}"#)
        .expect("parsed");
    assert_eq!(page.next_cursor, None);
    assert_eq!(page.completeness, ListingCompleteness::PossiblyIncomplete);
    assert!(page.completeness.disclosure().is_some());
    assert!(ListingCompleteness::Complete.disclosure().is_none());
}

// --------------------------------------------------------------- JSON output

#[test]
fn partial_json_is_reported_as_truncation_rather_than_as_malformed_output() {
    for cut in [
        "[",
        "[{",
        r#"[{"id""#,
        r#"[{"id":"rec_1""#,
        r#"[{"id":"rec_1"}"#,
        r#"{"files":[{"id":"rec_1"}]"#,
        "",
        "   \n  ",
    ] {
        assert_eq!(
            parser(ListingFormat::JsonArray, false).parse_listing(cut),
            Err(ParserError::Truncated),
            "{cut:?}"
        );
    }
}

#[test]
fn json_that_is_not_a_listing_at_all_is_malformed() {
    for text in [
        "not json at all",
        "123",
        "\"a string\"",
        "null",
        "true",
        r#"{"files":"not an array"}"#,
        r#"{"records":[{"id":"rec_1"}]}"#,
        "[[]]",
        "\u{1b}[32m[{\"id\":\"rec_1\"}]\u{1b}[0m",
        "]",
        "}",
    ] {
        assert_eq!(
            parser(ListingFormat::JsonArray, false).parse_listing(text),
            Err(ParserError::MalformedOutput),
            "{text:?}"
        );
    }
}

#[test]
fn a_record_field_of_the_wrong_type_fails_the_page() {
    for record in [
        r#"{"id":1}"#,
        r#"{"id":[]}"#,
        r#"{"id":"rec_1","duration_ms":"600000"}"#,
        r#"{"id":"rec_1","duration_ms":-1}"#,
        r#"{"id":"rec_1","duration_ms":1.5}"#,
        r#"{"id":"rec_1","duration_ms":18446744073709551616}"#,
        // A locale-formatted number is text, not a number.
        r#"{"id":"rec_1","size_bytes":"1 024"}"#,
        r#"{"id":"rec_1","size_bytes":"1.024"}"#,
        r#"{"id":"rec_1","has_transcript":"yes"}"#,
        r#"{"id":"rec_1","updated_at":7}"#,
        r#"{"id":"rec_1","created_at_ms":"2026-10-12"}"#,
        // A field this release never printed means the contract moved.
        r#"{"id":"rec_1","surprise":1}"#,
    ] {
        let stdout = format!("{record}\n");
        assert_eq!(
            parser(ListingFormat::JsonLines, false).parse_listing(&stdout),
            Err(ParserError::MalformedOutput),
            "{record}"
        );
    }
}

#[test]
fn numeric_fields_are_accepted_across_their_whole_declared_range() {
    let stdout = format!(
        "{}\n{}\n",
        r#"{"id":"rec_min","duration_ms":0,"size_bytes":0,"created_at_ms":-9223372036854775808}"#,
        r#"{"id":"rec_max","duration_ms":18446744073709551615,"size_bytes":18446744073709551615,"created_at_ms":9223372036854775807}"#,
    );
    let page = parser(ListingFormat::JsonLines, false)
        .parse_listing(&stdout)
        .expect("the extremes of each field are still in range");
    assert_eq!(page.items[0].duration_ms, Some(0));
    assert_eq!(page.items[0].captured_at_ms, Some(i64::MIN));
    assert_eq!(page.items[1].duration_ms, Some(u64::MAX));
    assert_eq!(page.items[1].declared_byte_length, Some(u64::MAX));
    assert_eq!(page.items[1].captured_at_ms, Some(i64::MAX));
}

#[test]
fn a_duplicate_json_key_is_refused_rather_than_resolved_by_last_wins() {
    let error = parser(ListingFormat::JsonLines, false)
        .parse_listing("{\"id\":\"rec_1\",\"id\":\"rec_2\"}\n")
        .expect_err("a repeated key is a contract change, not a preference");
    assert_eq!(
        error,
        ParserError::MalformedOutput,
        "a repeated key must not quietly become one record or two"
    );
}

#[test]
fn a_blank_identifier_fails_the_page_in_every_shape() {
    for record in [
        r#"{"name":"x"}"#,
        r#"{"id":null,"name":"x"}"#,
        r#"{"id":""}"#,
        r#"{"id":"   "}"#,
        "{\"id\":\"\\t\\n\"}",
    ] {
        let stdout = format!("{record}\n");
        assert_eq!(
            parser(ListingFormat::JsonLines, false).parse_listing(&stdout),
            Err(ParserError::MissingField { field: "id" }),
            "{record}"
        );
    }
}

#[test]
fn an_identifier_that_differs_only_in_padding_is_the_same_identity() {
    assert_eq!(
        parser(ListingFormat::JsonLines, false)
            .parse_listing("{\"id\":\"rec_1\"}\n{\"id\":\"  rec_1  \"}\n"),
        Err(ParserError::DuplicateExternalId),
        "trimming must happen before the uniqueness check, not after"
    );
}

#[test]
fn advisory_metadata_never_becomes_identity_or_a_false_claim() {
    let stdout = concat!(
        r#"{"id":" rec_1 ","title":"   ","name":"Board sync"}"#,
        "\n",
        r#"{"id":"rec_2","title":"Quarterly","updated_at":"   "}"#,
        "\n",
        r#"{"id":"rec_3","has_transcript":false}"#,
        "\n",
    );
    let page = parser(ListingFormat::JsonLines, false)
        .parse_listing(stdout)
        .expect("parsed");
    assert_eq!(external_ids(&page), ["rec_1", "rec_2", "rec_3"]);
    assert_eq!(
        page.items[0].display_name, "Untitled recording",
        "a blank title is not silently replaced by the name field"
    );
    assert_eq!(page.items[1].display_name, "Quarterly");
    assert_eq!(
        page.items[1].remote_version, None,
        "a blank modification marker is not a version"
    );
    assert!(!page.items[2].vendor_transcript_available);
    for item in &page.items {
        assert!(item.identity.is_stable());
        assert_eq!(item.identity.source_kind, SourceKind::PlaudCloudCli);
    }
}

#[test]
fn whitespace_only_json_lines_output_is_an_empty_page_not_an_error() {
    for stdout in ["", "\n", "   \n\t\n   ", "\n\n\n"] {
        let page = parser(ListingFormat::JsonLines, false)
            .parse_listing(stdout)
            .expect("blank lines carry no records");
        assert!(page.items.is_empty(), "{stdout:?}");
        assert_eq!(page.next_cursor, None);
    }
}

#[test]
fn one_bad_line_fails_the_whole_json_lines_page() {
    let stdout =
        "{\"id\":\"rec_1\"}\n\u{1b}[31merror: not logged in\u{1b}[0m\n{\"id\":\"rec_2\"}\n";
    assert_eq!(
        parser(ListingFormat::JsonLines, false).parse_listing(stdout),
        Err(ParserError::MalformedOutput),
        "a partial page is never shown as the listing"
    );

    let cut = "{\"id\":\"rec_1\"}\n{\"id\":\"rec_2\"\n";
    assert_eq!(
        parser(ListingFormat::JsonLines, false).parse_listing(cut),
        Err(ParserError::Truncated)
    );
}

#[test]
fn a_cursor_is_only_honoured_when_it_carries_something() {
    for (stdout, expected) in [
        (
            r#"{"files":[{"id":"rec_1"}],"next_cursor":"abc"}"#,
            Some("abc"),
        ),
        (r#"{"files":[{"id":"rec_1"}],"next_cursor":null}"#, None),
        (r#"{"files":[{"id":"rec_1"}],"next_cursor":""}"#, None),
        (r#"{"files":[{"id":"rec_1"}],"next_cursor":"  \t "}"#, None),
        (r#"{"files":[{"id":"rec_1"}]}"#, None),
        (r#"[{"id":"rec_1"}]"#, None),
    ] {
        let page = parser(ListingFormat::JsonArray, true)
            .parse_listing(stdout)
            .expect("parsed");
        assert_eq!(page.next_cursor.as_deref(), expected, "{stdout}");
    }
}

#[test]
fn more_records_than_we_will_materialize_fail_the_page() {
    let mut lines = String::with_capacity(MAX_RECORDS_PER_PAGE * 16);
    for index in 0..=MAX_RECORDS_PER_PAGE {
        let _ = writeln!(lines, "{{\"id\":\"rec_{index}\"}}");
    }
    assert_eq!(
        parser(ListingFormat::JsonLines, false).parse_listing(&lines),
        Err(ParserError::TooManyRecords {
            limit: MAX_RECORDS_PER_PAGE
        })
    );

    let mut array = String::with_capacity(MAX_RECORDS_PER_PAGE * 16);
    array.push('[');
    for index in 0..=MAX_RECORDS_PER_PAGE {
        if index > 0 {
            array.push(',');
        }
        let _ = write!(array, "{{\"id\":\"rec_{index}\"}}");
    }
    array.push(']');
    assert_eq!(
        parser(ListingFormat::JsonArray, false).parse_listing(&array),
        Err(ParserError::TooManyRecords {
            limit: MAX_RECORDS_PER_PAGE
        })
    );
}

// ------------------------------------------------------------ delimited table

#[test]
fn every_understood_table_column_is_read_and_an_unknown_one_is_ignored() {
    let stdout = "\
ID       Title        Duration   Size      Created At      Updated At   Transcript   Notes
-------  -----------  ---------  --------  --------------  -----------  -----------  -------
rec_1    Board sync   00:30:00   4096      1760000000000   rev-7        ✓            ignore
rec_2    Stand up     12:30      not-a-no  not-a-number    rev-8        0            ignore
";
    let page = parser(ListingFormat::DelimitedTable, false)
        .parse_listing(stdout)
        .expect("parsed");
    assert_eq!(external_ids(&page), ["rec_1", "rec_2"]);
    assert_eq!(page.items[0].display_name, "Board sync");
    assert_eq!(page.items[0].duration_ms, Some(1_800_000));
    assert_eq!(page.items[0].declared_byte_length, Some(4096));
    assert_eq!(page.items[0].captured_at_ms, Some(1_760_000_000_000));
    assert_eq!(page.items[0].remote_version.as_deref(), Some("rev-7"));
    assert!(page.items[0].vendor_transcript_available);
    // Cells we cannot read are left absent rather than guessed at zero.
    assert_eq!(page.items[1].duration_ms, Some(750_000));
    assert_eq!(page.items[1].declared_byte_length, None);
    assert_eq!(page.items[1].captured_at_ms, None);
    assert!(!page.items[1].vendor_transcript_available);
}

#[test]
fn every_affirmative_transcript_spelling_is_accepted_and_nothing_else_is() {
    for (cell, expected) in [
        ("yes", true),
        ("YES", true),
        ("true", true),
        ("y", true),
        ("1", true),
        ("✓", true),
        ("no", false),
        ("maybe", false),
        ("2", false),
        ("-", false),
    ] {
        let stdout = format!("ID       Transcript\nrec_1    {cell}\n");
        let page = parser(ListingFormat::DelimitedTable, false)
            .parse_listing(&stdout)
            .expect("parsed");
        assert_eq!(
            page.items[0].vendor_transcript_available, expected,
            "{cell:?}"
        );
    }
}

#[test]
fn a_single_column_table_is_read_without_inventing_neighbours() {
    let stdout = "File ID\n-------\nrec_1\nrec_2\n";
    let page = parser(ListingFormat::DelimitedTable, false)
        .parse_listing(stdout)
        .expect("parsed");
    assert_eq!(external_ids(&page), ["rec_1", "rec_2"]);
    assert_eq!(page.items[0].display_name, "Untitled recording");
}

#[test]
fn table_output_with_no_usable_header_is_refused() {
    for stdout in [
        "",
        "\n\n",
        "   \n  \t \n",
        "-------\n=======\n",
        "| --- | --- |\n",
        "Name       Duration\nBoard sync  00:30:00\n",
    ] {
        assert!(
            parser(ListingFormat::DelimitedTable, false)
                .parse_listing(stdout)
                .is_err(),
            "{stdout:?} must not be read as a listing"
        );
    }
}

#[test]
fn a_table_line_longer_than_the_cap_fails_the_page() {
    let long = "x".repeat(MAX_TABLE_LINE_BYTES + 1);
    let header = format!("ID    {long}");
    assert_eq!(
        parser(ListingFormat::DelimitedTable, false).parse_listing(&header),
        Err(ParserError::MalformedOutput),
        "an over-long header is refused before it is split"
    );

    let stdout = format!("ID    Name\nrec_1  {long}\n");
    assert_eq!(
        parser(ListingFormat::DelimitedTable, false).parse_listing(&stdout),
        Err(ParserError::MalformedOutput),
        "an over-long row is refused before it is split"
    );

    // Exactly at the cap is still read.
    let at_cap = "y".repeat(MAX_TABLE_LINE_BYTES - "rec_1  ".len());
    let stdout = format!("ID    Name\nrec_1  {at_cap}\n");
    assert!(parser(ListingFormat::DelimitedTable, false)
        .parse_listing(&stdout)
        .is_ok());
}

#[test]
fn a_table_row_that_does_not_match_the_header_shape_fails_the_page() {
    for stdout in [
        "ID       Name\nrec_1    Board sync    extra\n",
        "ID       Name\nrec_1\n",
        "ID       Name    Duration\nrec_1    Board sync\n",
    ] {
        assert_eq!(
            parser(ListingFormat::DelimitedTable, false).parse_listing(stdout),
            Err(ParserError::MalformedOutput),
            "{stdout:?}"
        );
    }
}

#[test]
fn a_table_duration_cell_is_read_in_every_printed_shape_and_no_other() {
    for (cell, expected) in [
        ("00:30:00", Some(1_800_000)),
        ("12:30", Some(750_000)),
        ("0", Some(0)),
        ("90", Some(90)),
        ("1:2:3:4", Some(223_384_000)),
        ("-", None),
        (":::", None),
        ("1h15m", None),
        ("99999999999999999999:00", None),
    ] {
        let stdout = format!("ID       Duration\nrec_1    {cell}\n");
        let page = parser(ListingFormat::DelimitedTable, false)
            .parse_listing(&stdout)
            .expect("the row shape is still valid");
        assert_eq!(page.items[0].duration_ms, expected, "{cell:?}");
    }
}

#[test]
fn more_table_rows_than_we_will_materialize_fail_the_page() {
    let mut stdout = String::with_capacity(MAX_RECORDS_PER_PAGE * 16);
    stdout.push_str("ID       Name\n");
    for index in 0..=MAX_RECORDS_PER_PAGE {
        let _ = writeln!(stdout, "rec_{index}    row");
    }
    assert_eq!(
        parser(ListingFormat::DelimitedTable, false).parse_listing(&stdout),
        Err(ParserError::TooManyRecords {
            limit: MAX_RECORDS_PER_PAGE
        })
    );
}

// --------------------------------------------------------- the 0.3.x listing

#[test]
fn a_page_with_less_framing_than_the_release_prints_is_truncated() {
    for stdout in [
        "",
        "\n\n",
        "Files on this page: 1\n",
        "Files on this page: 1\n\nID    NAME    DATE    DURATION\n",
    ] {
        assert_eq!(
            plaud_parser().parse_listing(stdout),
            Err(ParserError::Truncated),
            "{stdout:?}"
        );
    }
}

#[test]
fn a_count_line_the_release_never_printed_fails_the_page() {
    let row = one_plaud_row();
    let full = plaud_page(std::slice::from_ref(&row), 1, None);
    for broken in [
        full.replace("Files on this page: 1", "Files on this page: many"),
        full.replace("Files on this page: 1", "Files on this page: -1"),
        full.replace("Files on this page: 1", "Files on this page: 1.0"),
        full.replace("Files on this page: 1", "Files on this page:"),
        full.replace("Files on this page: 1", "Files: 1"),
        // Colour escapes mean the line no longer starts with the known prefix.
        full.replace(
            "Files on this page: 1",
            "\u{1b}[1mFiles on this page: 1\u{1b}[0m",
        ),
    ] {
        assert_eq!(
            plaud_parser().parse_listing(&broken),
            Err(ParserError::MalformedOutput),
            "{broken:?}"
        );
    }
}

#[test]
fn a_declared_count_beyond_the_record_cap_is_refused_before_the_rows_are_read() {
    let stdout = plaud_page(&[], 1, Some(MAX_RECORDS_PER_PAGE + 1));
    assert_eq!(
        plaud_parser().parse_listing(&stdout),
        Err(ParserError::TooManyRecords {
            limit: MAX_RECORDS_PER_PAGE
        })
    );
}

#[test]
fn a_line_longer_than_the_cap_fails_the_0_3_page() {
    let row = plaud_row_text(
        "of_1a2b3c4d5e6f708192a3b4c5d6e7f801",
        &"n".repeat(MAX_TABLE_LINE_BYTES),
        "2026-10-12",
        "41m07s",
    );
    assert_eq!(
        plaud_parser().parse_listing(&plaud_page(&[row], 1, None)),
        Err(ParserError::MalformedOutput)
    );
}

#[test]
fn a_row_count_that_disagrees_with_the_rows_is_never_reconciled() {
    let row = one_plaud_row();
    assert_eq!(
        plaud_parser().parse_listing(&plaud_page(std::slice::from_ref(&row), 1, Some(2))),
        Err(ParserError::Truncated),
        "a count larger than the rows means rows went missing"
    );
    assert_eq!(
        plaud_parser().parse_listing(&plaud_page(std::slice::from_ref(&row), 1, Some(0))),
        Err(ParserError::MalformedOutput),
        "a count smaller than the rows means something was added"
    );
}

#[test]
fn a_page_footer_that_is_not_a_page_number_fails_the_page() {
    let row = one_plaud_row();
    let full = plaud_page(std::slice::from_ref(&row), 1, None);
    for broken in [
        full.replace("Page 1", "Page one"),
        full.replace("Page 1", "Page"),
        full.replace("Page 1", "Page -1"),
        full.replace("Page 1", "Page 4294967296"),
        full.replace("Page 1", "Pages 1"),
        // A 1-based counter never starts at zero.
        full.replace("Page 1", "Page 0"),
    ] {
        assert_eq!(
            plaud_parser().parse_listing(&broken),
            Err(ParserError::MalformedOutput),
            "{broken:?}"
        );
    }
}

#[test]
fn the_last_representable_page_does_not_request_a_further_one() {
    let row = one_plaud_row();
    let page = plaud_parser()
        .parse_listing(&plaud_page(std::slice::from_ref(&row), u32::MAX, None))
        .expect("the page itself is well formed");
    assert_eq!(page.items.len(), 1);
    assert_eq!(
        page.next_cursor, None,
        "a page number we cannot increment is the end, not a wrap to zero"
    );

    let page = plaud_parser()
        .parse_listing(&plaud_page(std::slice::from_ref(&row), u32::MAX - 1, None))
        .expect("parsed");
    assert_eq!(page.next_cursor.as_deref(), Some("4294967295"));
}

#[test]
fn an_empty_page_ends_enumeration_without_a_cursor() {
    let page = plaud_parser()
        .parse_listing(&plaud_page(&[], 999, None))
        .expect("parsed");
    assert!(page.items.is_empty(), "an empty page lists nothing");
    assert_eq!(page.next_cursor, None);
    assert_eq!(page.completeness, ListingCompleteness::Complete);
}

#[test]
fn a_header_whose_columns_are_not_whole_words_is_refused() {
    let row = one_plaud_row();
    let full = plaud_page(std::slice::from_ref(&row), 1, None);
    for broken in [
        // The token is there, but glued to another word on the right.
        full.replace("ID  ", "IDX "),
        // ... and on the left.
        full.replace("  ID ", " XID "),
        // A renamed column.
        full.replace("DURATION", "LENGTH  "),
        full.replace("DATE", "DAY "),
        // A column whose name changed by one character.
        full.replace("DATE", "DAT3"),
    ] {
        assert_eq!(
            plaud_parser().parse_listing(&broken),
            Err(ParserError::MalformedOutput),
            "{broken:?}"
        );
    }
}

#[test]
fn a_rule_the_release_does_not_draw_is_refused() {
    let row = one_plaud_row();
    let full = plaud_page(std::slice::from_ref(&row), 1, None);
    for broken in [
        full.replace('\u{2500}', "-"),
        full.replace('\u{2500}', "="),
        full.replacen('\u{2500}', "x", 1),
    ] {
        assert_eq!(
            plaud_parser().parse_listing(&broken),
            Err(ParserError::MalformedOutput)
        );
    }
}

#[test]
fn an_identifier_we_could_not_hand_back_to_the_tool_fails_the_page() {
    for id in [
        "",
        "   ",
        "of_1a2b; rm -rf /",
        "of_1a2b$(whoami)",
        "of 1a2b",
        "-rf",
        "--help",
        "of_1a2b\u{2026}",
        "of/1a2b",
    ] {
        let rows = [plaud_row_text(id, "name", "2026-10-12", "41m07s")];
        assert_eq!(
            plaud_parser().parse_listing(&plaud_page(&rows, 1, None)),
            Err(ParserError::MissingField { field: "id" }),
            "{id:?} must not be handed back to the CLI"
        );
    }

    // The charset the CLI argument type accepts is read without complaint.
    for id in ["of_1a2b", "OF.1a2b", "of:1a2b", "of-1a2b", "0"] {
        let rows = [plaud_row_text(id, "name", "2026-10-12", "41m07s")];
        assert!(
            plaud_parser()
                .parse_listing(&plaud_page(&rows, 1, None))
                .is_ok(),
            "{id:?} is a usable identifier"
        );
    }
}

#[test]
fn a_row_shorter_than_its_columns_fails_rather_than_reading_empty_cells() {
    for row in ["x", "  ", "  of_1a2b"] {
        let rows = [row.to_owned()];
        assert!(
            plaud_parser()
                .parse_listing(&plaud_page(&rows, 1, None))
                .is_err(),
            "{row:?} must not become a record"
        );
    }
}

#[test]
fn a_date_cell_is_validated_as_a_calendar_date_not_merely_as_digits() {
    for date in [
        "",
        "yesterday",
        "2026-13-01",
        "2026-00-01",
        "2026-10-00",
        "2026-10-32",
        "2026-1-01",
        "2026-01-1",
        "20260-01-01",
        "2026-01-01-01",
        "2026/01/01",
        "abcd-01-01",
        "2026-ab-01",
        "-2026-01",
    ] {
        let rows = [plaud_row_text("of_1a2b", "name", date, "41m07s")];
        assert_eq!(
            plaud_parser().parse_listing(&plaud_page(&rows, 1, None)),
            Err(ParserError::MalformedOutput),
            "{date:?} is not a date this release prints"
        );
    }

    for date in ["2026-01-01", "2026-12-31", "0000-01-01", "9999-12-31"] {
        let rows = [plaud_row_text("of_1a2b", "name", date, "41m07s")];
        assert!(
            plaud_parser()
                .parse_listing(&plaud_page(&rows, 1, None))
                .is_ok(),
            "{date:?} is a date"
        );
    }
}

#[test]
fn the_duration_grammar_is_exactly_the_one_this_release_prints() {
    for duration in [
        "",
        "about an hour",
        "01:15:00",
        "1h 15m",
        "15m1h",
        "1h15",
        "-5s",
        "1.5h",
        "1234567890s",
        "41m07s41m",
        "s",
    ] {
        let rows = [plaud_row_text("of_1a2b", "name", "2026-10-12", duration)];
        assert_eq!(
            plaud_parser().parse_listing(&plaud_page(&rows, 1, None)),
            Err(ParserError::MalformedOutput),
            "{duration:?} is not a duration this release prints"
        );
    }

    for (duration, expected) in [
        ("0s", 0u64),
        ("45s", 45_000),
        ("1m03s", 63_000),
        ("1h15m", 4_500_000),
        ("41m07s", 2_467_000),
        ("1h02m03s", 3_723_000),
        ("999999999h", 3_599_999_996_400_000),
    ] {
        let rows = [plaud_row_text("of_1a2b", "name", "2026-10-12", duration)];
        let page = plaud_parser()
            .parse_listing(&plaud_page(&rows, 1, None))
            .expect("a readable duration");
        assert_eq!(page.items[0].duration_ms, Some(expected), "{duration:?}");
    }
}

#[test]
fn a_row_without_a_name_is_still_a_recording() {
    let rows = [plaud_row_text(
        "of_1a2b3c4d5e6f708192a3b4c5d6e7f801",
        "",
        "2026-10-12",
        "41m07s",
    )];
    let page = plaud_parser()
        .parse_listing(&plaud_page(&rows, 1, None))
        .expect("parsed");
    assert_eq!(page.items[0].display_name, "Untitled recording");
    assert_eq!(page.items[0].captured_at_ms, None);
    assert_eq!(page.items[0].declared_byte_length, None);
    assert_eq!(page.items[0].remote_version, None);
}

#[test]
fn colour_escapes_inside_a_name_stay_display_text_and_never_identity() {
    let rows = [plaud_row_text(
        "of_1a2b3c4d5e6f708192a3b4c5d6e7f801",
        "\u{1b}[31mBoard sync\u{1b}[0m",
        "2026-10-12",
        "41m07s",
    )];
    let page = plaud_parser()
        .parse_listing(&plaud_page(&rows, 1, None))
        .expect("the row geometry is unchanged");
    assert_eq!(
        external_ids(&page),
        ["of_1a2b3c4d5e6f708192a3b4c5d6e7f801"],
        "the identifier is unaffected by decoration in another column"
    );
    assert!(page.items[0].display_name.contains('\u{1b}'));
}

#[test]
fn the_same_identifier_twice_fails_the_0_3_page() {
    let row = one_plaud_row();
    let rows = [row.clone(), row];
    assert_eq!(
        plaud_parser().parse_listing(&plaud_page(&rows, 1, None)),
        Err(ParserError::DuplicateExternalId)
    );
}

// -------------------------------------------------------------- error mapping

#[test]
fn every_parser_error_maps_onto_a_stable_source_error() {
    assert_eq!(
        SourceError::from(ParserError::UnvalidatedVersion {
            reported: "9.9.9".to_owned()
        }),
        SourceError::CliUnsupported
    );
    for error in [
        ParserError::MalformedOutput,
        ParserError::MissingField { field: "id" },
        ParserError::DuplicateExternalId,
        ParserError::Truncated,
        ParserError::TooManyRecords {
            limit: MAX_RECORDS_PER_PAGE,
        },
    ] {
        assert_eq!(
            SourceError::from(error.clone()),
            SourceError::MalformedOutput
        );
        assert!(
            !error.to_string().is_empty(),
            "every rejection carries copy we wrote"
        );
    }
}
