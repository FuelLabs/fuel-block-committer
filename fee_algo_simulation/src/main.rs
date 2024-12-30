use std::{ops::RangeInclusive, path::PathBuf};

use anyhow::Result;
use itertools::Itertools;
use serde::{Deserialize, Serialize};
use services::{
    block_importer::port::fuel::__mock_MockApi_Api::__compressed_blocks_in_height_range,
    historical_fees::{
        self,
        port::{
            cache::CachingApi,
            l1::{Api, BlockFees, Fees, SequentialBlockFees},
        },
        service::{calculate_blob_tx_fee, HistoricalFees, SmaPeriods},
    },
    state_committer::{AlgoConfig, FeeThresholds},
};
use xdg::BaseDirectories;

#[derive(Debug, Serialize, Deserialize, Default)]
struct SavedFees {
    fees: Vec<BlockFees>,
}

const URL: &str = "https://eth.llamarpc.com";

pub struct PersistentApi<P> {
    provider: P,
}

fn fee_file() -> PathBuf {
    let xdg = BaseDirectories::with_prefix("fee_simulation").unwrap();
    if let Some(cache) = xdg.find_cache_file("fee_cache.json") {
        cache
    } else {
        xdg.place_data_file("fee_cache.json").unwrap()
    }
}

fn load_cache() -> Vec<(u64, Fees)> {
    let Ok(contents) = std::fs::read_to_string(fee_file()) else {
        return vec![];
    };

    let fees: SavedFees = serde_json::from_str(&contents).unwrap_or_default();

    fees.fees.into_iter().map(|f| (f.height, f.fees)).collect()
}

fn save_cache(cache: impl IntoIterator<Item = (u64, Fees)>) -> anyhow::Result<()> {
    let fees = SavedFees {
        fees: cache
            .into_iter()
            .map(|(height, fees)| BlockFees { height, fees })
            .collect(),
    };

    std::fs::write(fee_file(), serde_json::to_string(&fees)?)?;

    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let client = eth::HttpClient::new(URL).unwrap();
    let num_blocks_per_month = 30 * 24 * 3600 / 12;

    let caching_api = CachingApi::new(client, num_blocks_per_month * 2);
    caching_api.import(load_cache()).await;

    let historical_fees = HistoricalFees::new(caching_api.clone());

    let ending_height = 21514918u64;
    let amount_of_blocks = num_blocks_per_month;

    let config = AlgoConfig {
        sma_periods: SmaPeriods {
            short: 25.try_into().unwrap(),
            long: 300.try_into().unwrap(),
        },
        fee_thresholds: FeeThresholds {
            max_l2_blocks_behind: (8 * 3600).try_into().unwrap(),
            start_discount_percentage: 0.10.try_into().unwrap(),
            end_premium_percentage: 0.20.try_into().unwrap(),
            always_acceptable_fee: 1000000000000000,
        },
    };

    let fees = caching_api
        .fees(ending_height - amount_of_blocks as u64..=ending_height)
        .await?;

    plot_fees(fees)?;

    save_cache(caching_api.export().await)?;

    Ok(())
}

use plotters::prelude::*;

const OUT_FILE_NAME: &str = "sample.png";
fn plot_fees(fees: SequentialBlockFees) -> Result<()> {
    let root_area = BitMapBackend::new(OUT_FILE_NAME, (1024, 768)).into_drawing_area();

    root_area.fill(&WHITE)?;

    let root_area = root_area.titled("Fees", ("sans-serif", 60))?;

    let fees: Vec<_> = fees
        .into_iter()
        .map(|block_fees| {
            (
                block_fees.height,
                calculate_blob_tx_fee(6, &block_fees.fees),
            )
        })
        .collect_vec();

    let min_height = fees.first().unwrap().0;
    let max_height = fees.last().unwrap().0;

    let max_fee = fees.iter().map(|(_, fees)| fees).max().unwrap();

    let mut cc = ChartBuilder::on(&root_area)
        .margin(50)
        .set_left_and_bottom_label_area_size(50)
        // .set_all_label_area_size(50)
        .build_cartesian_2d(min_height..max_height + 1, 0..max_fee.saturating_mul(2))?;

    cc.configure_mesh()
        .x_labels(20)
        .y_labels(10)
        .disable_mesh()
        .x_label_formatter(&|v| format!("{:.1}", v))
        .y_label_formatter(&|v| format!("{:.3}", (*v as f64 / 10f64.powi(18))))
        .draw()?;

    cc.draw_series(LineSeries::new(fees, &RED))?
        .label("Current fees")
        .legend(|(x, y)| PathElement::new(vec![(x, y), (x + 20, y)], RED));

    // cc.draw_series(LineSeries::new(
    //     x_axis.values().map(|x| (x, x.cos())),
    //     &BLUE,
    // ))?
    // .label("Cosine")
    // .legend(|(x, y)| PathElement::new(vec![(x, y), (x + 20, y)], BLUE));

    cc.configure_series_labels().border_style(BLACK).draw()?;

    /*
    // It's possible to use a existing pointing element
     cc.draw_series(PointSeries::<_, _, Circle<_>>::new(
        (-3.0f32..2.1f32).step(1.0).values().map(|x| (x, x.sin())),
        5,
        Into::<ShapeStyle>::into(&RGBColor(255,0,0)).filled(),
    ))?;*/

    // Otherwise you can use a function to construct your pointing element yourself
    // cc.draw_series(PointSeries::of_element(
    //     (-3.0f32..2.1f32).step(1.0).values().map(|x| (x, x.sin())),
    //     5,
    //     ShapeStyle::from(&RED).filled(),
    //     &|coord, size, style| {
    //         EmptyElement::at(coord)
    //             + Circle::new((0, 0), size, style)
    //             + Text::new(format!("{:?}", coord), (0, 15), ("sans-serif", 15))
    //     },
    // ))?;

    // let drawing_areas = lower.split_evenly((1, 2));

    // for (drawing_area, idx) in drawing_areas.iter().zip(1..) {
    //     let mut cc = ChartBuilder::on(drawing_area)
    //         .x_label_area_size(30)
    //         .y_label_area_size(30)
    //         .margin_right(20)
    //         .caption(format!("y = x^{}", 1 + 2 * idx), ("sans-serif", 40))
    //         .build_cartesian_2d(-1f32..1f32, -1f32..1f32)?;
    //     cc.configure_mesh()
    //         .x_labels(5)
    //         .y_labels(3)
    //         .max_light_lines(4)
    //         .draw()?;
    //
    //     cc.draw_series(LineSeries::new(
    //         (-1f32..1f32)
    //             .step(0.01)
    //             .values()
    //             .map(|x| (x, x.powf(idx as f32 * 2.0 + 1.0))),
    //         &BLUE,
    //     ))?;
    // }

    // To avoid the IO failure being ignored silently, we manually call the present function
    root_area.present().expect("Unable to write result to file, please make sure 'plotters-doc-data' dir exists under current dir");
    println!("Result has been saved to {}", OUT_FILE_NAME);
    Ok(())
}
