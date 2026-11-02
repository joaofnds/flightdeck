use super::*;
use crate::event_table::EventTable;

#[test]
fn legacy_source_requires_every_base_field_before_an_empty_query_plan() {
    // Goal: synthetic query fields must all exist in the admitted physical schema.
    // Method: omit each mandatory catalog column from an otherwise valid empty source.
    for field in model::catalog::EVENTS.columns {
        let mut input = frame(0);
        input.drop_in_place(field.name).unwrap();

        refused_plan(input, "event base field");
    }
}

#[test]
fn legacy_source_requires_the_base_publication_type_before_an_empty_query_plan() {
    // Goal: empty physical rows cannot disguise an unsupported publication storage type.
    // Method: replace the UInt64 publication field with UInt8 before source admission.
    let mut input = frame(0);
    input
        .with_column(Column::new("publication".into(), Vec::<u8>::new()))
        .unwrap();

    refused_plan(input, "event base storage type");
}

#[test]
fn legacy_source_refuses_metadata_selected_binary_offsets_before_query_planning() {
    // Goal: an empty identity cannot hide a native storage type the semantic owner refuses.
    // Method: write each reserved binary identity field with native maintain_type metadata.
    for name in ["executable_name", "app_path"] {
        let mut input = frame(0);
        input
            .with_column(
                input
                    .column(name)
                    .unwrap()
                    .cast(&DataType::BinaryOffset)
                    .unwrap(),
            )
            .unwrap();
        let fixture = Fixture::with_frame(input.clone(), None);
        let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
        let path = table(&fixture, &budget).part(0).unwrap().path().to_owned();
        let schema = input
            .schema()
            .iter()
            .map(|(name, kind)| {
                (
                    name.clone(),
                    kind.to_arrow_field(name.clone(), CompatLevel::newest()),
                )
            })
            .collect();
        let metadata = polars_parquet::write::schema_to_metadata_key(&schema);
        let file = collector::recording::private::open_private_file_writable(&path).unwrap();
        file.set_len(0).unwrap();
        ParquetWriter::new(file)
            .with_key_value_metadata(Some(KeyValueMetadata::Static(vec![metadata])))
            .finish(&mut input)
            .unwrap();
        let native = ParquetReader::new(std::fs::File::open(&path).unwrap())
            .finish()
            .unwrap();
        assert_eq!(
            native.column(name).unwrap().dtype(),
            &DataType::BinaryOffset
        );

        refused_fixture_plan(&fixture, "recording storage type");
    }
}

#[test]
fn legacy_source_refuses_case_variant_identity_fields_before_projection() {
    // Goal: projection cannot discard a reserved identity spelling before admission sees it.
    // Method: change only the version field's case in an otherwise valid empty source.
    let mut input = frame(0);
    input
        .rename("identity_version", "Identity_Version".into())
        .unwrap();

    refused_plan(input, "process identity field spelling");
}

#[test]
fn legacy_source_refuses_extra_columns_before_projection() {
    // Goal: omitting unknown fields cannot evade the existing complete-table column bound.
    // Method: append one unknown scalar to the complete legacy event schema.
    let mut input = frame(0);
    input
        .with_column(Column::new("unknown".into(), Vec::<u8>::new()))
        .unwrap();

    refused_plan(input, "table column count");
}

#[test]
fn legacy_source_refuses_partial_identity_extensions_before_projection() {
    // Goal: source admission requires the existing identity extension to be present whole.
    // Method: remove one member from an otherwise complete empty legacy file.
    let mut input = frame(0);
    input.drop_in_place("identity_version").unwrap();

    refused_plan(input, "incomplete process identity extension");
}

#[test]
fn legacy_source_refuses_partial_provenance_extensions_before_projection() {
    // Goal: empty physical rows do not excuse an incomplete provenance group.
    // Method: append only its version field to a real legacy schema before planning.
    let mut input = frame(0);
    input
        .with_column(Column::new(
            "event_observation_version".into(),
            Vec::<u8>::new(),
        ))
        .unwrap();

    refused_plan(input, "incomplete event observation group");
}

fn refused_plan(input: DataFrame, expected: &str) {
    let fixture = Fixture::with_frame(input, None);
    refused_fixture_plan(&fixture, expected);
}

fn refused_fixture_plan(fixture: &Fixture, expected: &str) {
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let result = table(fixture, &budget).into_lazy(&budget);
    assert!(matches!(
        result,
        Err(AdmittedReadRefusal::Decode(Error::Bound(message))) if message == expected
    ));
    assert_eq!(budget.used(), 0);
}

#[test]
fn legacy_source_checks_reserved_identity_types_before_an_empty_query_plan() {
    // Goal: LIMIT0 cannot bypass the source's complete identity type admission.
    // Method: open an empty canonical source whose identity version has the wrong scalar type.
    let mut input = frame(0);
    input
        .with_column(Column::new("identity_version".into(), Vec::<i64>::new()))
        .unwrap();
    let fixture = Fixture::with_frame(input, None);
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));

    let result = table(&fixture, &budget).into_lazy(&budget);

    assert!(matches!(
        result,
        Err(AdmittedReadRefusal::Decode(Error::Bound(
            "process identity storage type"
        )))
    ));
    assert_eq!(budget.used(), 0);
}

#[test]
fn legacy_source_checks_reserved_provenance_types_before_an_empty_query_plan() {
    // Goal: an empty provenance group must retain its exact UInt8 storage contract.
    // Method: add both correctly spelled fields with one Int64 type before source planning.
    let mut input = frame(0);
    input
        .with_column(Column::new(
            "event_observation_version".into(),
            Vec::<i64>::new(),
        ))
        .unwrap();
    input
        .with_column(Column::new(
            "event_observation_availability".into(),
            Vec::<u8>::new(),
        ))
        .unwrap();
    let fixture = Fixture::with_frame(input, None);
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));

    let result = table(&fixture, &budget).into_lazy(&budget);

    assert!(matches!(
        result,
        Err(AdmittedReadRefusal::Decode(Error::Bound(
            "event observation field"
        )))
    ));
    assert_eq!(budget.used(), 0);
}

#[test]
fn legacy_direct_count_admits_an_empty_physical_file() {
    // Goal: a manifest's zero rows cannot make corrupt physical bytes disappear.
    // Method: replace the committed empty file with malformed bytes, then use direct count.
    let fixture = Fixture::with_rows(0);
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let path = table(&fixture, &budget).part(0).unwrap().path().to_owned();
    std::fs::write(path, b"not a parquet file").unwrap();
    let source = table(&fixture, &budget);

    let result = source.count_budgeted(None, &budget);

    assert!(result.is_err());
    drop(source);
    assert_eq!(budget.used(), 0);
}

#[test]
fn legacy_direct_count_compares_physical_rows_with_a_zero_manifest() {
    // Goal: valid nonempty physical bytes cannot be reported as a successful empty relation.
    // Method: write one real row over an empty file before acquiring the canonical source pin.
    let fixture = Fixture::with_rows(0);
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let path = table(&fixture, &budget).part(0).unwrap().path().to_owned();
    std::fs::remove_file(&path).unwrap();
    crate::encoder::encode_write(&mut frame(1), crate::encoder::encode_open(&path).unwrap())
        .unwrap();
    let source = table(&fixture, &budget);

    let result = source.count_budgeted(None, &budget);

    assert!(matches!(
        result,
        Err(AdmittedReadRefusal::Decode(Error::Bound(
            "event physical row count"
        )))
    ));
    drop(source);
    assert_eq!(budget.used(), 0);
}

fn table(fixture: &Fixture, budget: &Arc<ByteBudget>) -> EventTable {
    let recording = collector::recording::read_identity(&fixture.0).unwrap();
    let marker = crate::layout::WindowKey::from(&identity(recording)).marker(&fixture.0);
    let manifest = crate::read_committed(&fixture.0, &marker).unwrap();
    EventTable::open_budgeted(&fixture.0, &manifest, budget).unwrap()
}
