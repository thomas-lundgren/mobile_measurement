use std::path::PathBuf;
use fieldtest_core::log_writer::{SegmentedPacketWriter, PacketRow, Direction};
use fieldtest_core::timestamp::Timestamp;

fn main() {
    let dir = PathBuf::from("/tmp/test_parquet");
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    {
        let mut w = SegmentedPacketWriter::new(&dir, 100).unwrap();
        w.append(&PacketRow {
            flow_id: 1,
            sequence_number: 1,
            direction: Direction::Sent,
            timestamp: Timestamp::now(),
            payload_size: 64,
        }).unwrap();
        println!("Appended 1 row");
    }
    println!("Writer dropped");
    let files: Vec<_> = std::fs::read_dir(&dir).unwrap()
        .filter_map(|e| e.ok())
        .map(|e| (e.path(), e.metadata().unwrap().len()))
        .collect();
    for (p, s) in &files {
        println!("{:?}: {} bytes", p, s);
    }
}
