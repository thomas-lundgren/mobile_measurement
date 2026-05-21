use fieldtest_core::log_writer::{SegmentedPacketWriter, PacketRow, Direction};
use fieldtest_core::timestamp::Timestamp;
use std::path::PathBuf;

fn main() {
    let dir = PathBuf::from("/tmp/test_direct_logs");
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    {
        let mut w = SegmentedPacketWriter::new(&dir, 10).unwrap();
        for i in 0..25u64 {
            w.append(&PacketRow {
                flow_id: 1,
                sequence_number: i,
                direction: if i % 2 == 0 { Direction::Sent } else { Direction::Received },
                timestamp: Timestamp::now(),
                payload_size: 64,
            }).unwrap();
        }
        println!("Appended 25 rows");
    }
    let files: Vec<_> = std::fs::read_dir(&dir).unwrap()
        .filter_map(|e| e.ok())
        .map(|e| (e.path(), e.metadata().unwrap().len()))
        .collect();
    for (p, s) in &files {
        println!("{:?}: {} bytes", p, s);
    }
}
