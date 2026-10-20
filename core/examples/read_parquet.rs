use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use std::fs::File;

fn main() {
    let path = std::env::args().nth(1).expect("need path");
    let file = File::open(&path).expect("open failed");
    let reader = ParquetRecordBatchReaderBuilder::try_new(file)
        .expect("builder failed")
        .build()
        .expect("build failed");
    let mut total = 0;
    for b in reader {
        total += b.unwrap().num_rows();
    }
    println!("rows: {}", total);
}
