use futures::StreamExt;
use itertools::Itertools;
use mockall::{predicate::eq, Sequence};
use services::{
    block_bundler::port::Storage,
    block_importer::service::BlockImporter,
    types::{nonempty, CollectNonEmpty},
    Result, Runner,
};
use test_helpers::Blocks;

#[tokio::test]
async fn imports_first_block_when_db_is_empty() -> Result<()> {
    // given
    let setup = test_helpers::Setup::init().await;

    let block = test_helpers::mocks::fuel::generate_block(0, 100);

    let fuel_mock = test_helpers::mocks::fuel::these_blocks_exist(vec![block.clone()], true);

    let mut importer = BlockImporter::new(setup.db(), fuel_mock, 0);

    // when
    importer.run().await?;

    // then
    let all_blocks = setup
        .db()
        .lowest_sequence_of_unbundled_blocks(0, u32::MAX)
        .await?
        .unwrap();

    let expected_block = nonempty![block];

    assert_eq!(all_blocks.into_inner(), expected_block);

    Ok(())
}

#[tokio::test]
async fn does_not_request_or_import_blocks_already_in_db() -> Result<()> {
    // given
    let setup = test_helpers::Setup::init().await;

    let block_size = 100;
    let existing_blocks = setup
        .import_blocks(Blocks::WithHeights {
            range: 0..=2,
            block_size,
        })
        .await;

    let new_blocks = (3..=5)
        .map(|height| test_helpers::mocks::fuel::generate_block(height, 100))
        .collect_vec();

    let all_blocks = existing_blocks
        .into_iter()
        .chain(new_blocks.clone())
        .collect_nonempty()
        .unwrap();

    let fuel_mock = test_helpers::mocks::fuel::these_blocks_exist(new_blocks.clone(), true);

    let mut importer = BlockImporter::new(setup.db(), fuel_mock, 1000);

    // when
    importer.run().await?;

    // then
    let stored_blocks = setup
        .db()
        .lowest_sequence_of_unbundled_blocks(0, u32::MAX)
        .await?
        .unwrap();

    pretty_assertions::assert_eq!(stored_blocks.into_inner(), all_blocks);

    Ok(())
}

#[tokio::test]
async fn respects_height_even_if_blocks_before_are_missing() -> Result<()> {
    // given
    let setup = test_helpers::Setup::init().await;

    let block_size = 100;
    setup
        .import_blocks(Blocks::WithHeights {
            range: 0..=2,
            block_size,
        })
        .await;

    let starting_height = 8;
    let new_blocks = (starting_height..=13)
        .map(|height| test_helpers::mocks::fuel::generate_block(height, 100))
        .collect_nonempty()
        .unwrap();

    let fuel_mock = test_helpers::mocks::fuel::these_blocks_exist(new_blocks.clone(), true);

    let mut importer = BlockImporter::new(setup.db(), fuel_mock, 5);

    // when
    importer.run().await?;

    // then
    let stored_new_blocks = setup
        .db()
        .lowest_sequence_of_unbundled_blocks(starting_height, u32::MAX)
        .await?
        .unwrap();

    pretty_assertions::assert_eq!(stored_new_blocks.into_inner(), new_blocks);

    Ok(())
}

#[tokio::test]
async fn handles_chain_with_no_new_blocks() -> Result<()> {
    // given
    let setup = test_helpers::Setup::init().await;

    let fuel_blocks = setup
        .import_blocks(Blocks::WithHeights {
            range: 0..=2,
            block_size: 100,
        })
        .await;

    let fuel_mock = test_helpers::mocks::fuel::these_blocks_exist(fuel_blocks.clone(), true);

    let mut importer = BlockImporter::new(setup.db(), fuel_mock, 0);

    // when
    importer.run().await?;

    // then
    // Database should remain unchanged
    let stored_blocks = setup
        .db()
        .lowest_sequence_of_unbundled_blocks(0, u32::MAX)
        .await?
        .unwrap();

    assert_eq!(stored_blocks.into_inner(), fuel_blocks);

    Ok(())
}

#[tokio::test]
async fn skips_blocks_outside_lookback_window() -> Result<()> {
    // given
    let setup = test_helpers::Setup::init().await;
    let lookback_window = 2;

    let blocks_to_import =
        (3..=5).map(|height| test_helpers::mocks::fuel::generate_block(height, 100));

    let fuel_mock = test_helpers::mocks::fuel::these_blocks_exist(blocks_to_import, true);

    let mut importer = BlockImporter::new(setup.db(), fuel_mock, lookback_window);

    // when
    importer.run().await?;

    // then
    let unbundled_blocks = setup
        .db()
        .lowest_sequence_of_unbundled_blocks(0, u32::MAX)
        .await?
        .unwrap();

    let unbundled_block_heights: Vec<_> = unbundled_blocks
        .into_inner()
        .iter()
        .map(|b| b.height)
        .collect();

    assert_eq!(
        unbundled_block_heights,
        vec![3, 4, 5],
        "Blocks outside the lookback window should remain unbundled"
    );

    Ok(())
}

#[tokio::test]
async fn fills_in_missing_blocks_inside_lookback_window() -> Result<()> {
    // given
    let setup = test_helpers::Setup::init().await;

    for range in [(3..=10), (40..=60)] {
        setup
            .import_blocks(Blocks::WithHeights {
                range,
                block_size: 100,
            })
            .await;
    }

    let mut fuel_mock = services::block_importer::port::fuel::MockApi::new();

    let mut sequence = Sequence::new();

    for range in [0..=2, 11..=39, 61..=100] {
        fuel_mock
            .expect_compressed_blocks_in_height_range()
            .with(eq(range))
            .once()
            .in_sequence(&mut sequence)
            .return_once(move |range| {
                let blocks =
                    range.map(|height| Ok(test_helpers::mocks::fuel::generate_block(height, 100)));

                futures::stream::iter(blocks).boxed()
            });
    }

    fuel_mock
        .expect_latest_height()
        .once()
        .return_once(|| Box::pin(async { Ok(100) }));

    let mut importer = BlockImporter::new(setup.db(), fuel_mock, 101);

    // when
    importer.run().await?;

    // then
    let unbundled_blocks = setup
        .db()
        .lowest_sequence_of_unbundled_blocks(0, u32::MAX)
        .await?
        .unwrap();

    let unbundled_block_heights: Vec<_> = unbundled_blocks
        .into_inner()
        .iter()
        .map(|b| b.height)
        .collect();

    assert_eq!(
        unbundled_block_heights,
        (0..=100).collect_vec(),
        "Blocks outside the lookback window should remain unbundled"
    );

    Ok(())
}
