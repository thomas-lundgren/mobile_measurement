use std::sync::Mutex;
use fieldtest_core::log_writer::{SegmentedPacketWriter, PacketRow, Direction};
use fieldtest_core::timestamp::Timestamp;
use std::path::PathBuf;

fn main() {
    let dir = PathBuf::from("/tmp/test_thread_parquet");
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    
    let writer: Option<SegmentedPacketWriter> = Some(SegmentedPacketWriter::new(&dir, 100).unwrap());
    let writer = Mutex::new(writer);
    
    {
        let mut w = writer.lock().unwrap();
        if let Some(ref mut wr) = *w {
            wr.append(&PacketRow {
                flow_id: 1,
                sequence_number: 1,
                direction: Direction::Sent,
                timestamp: Timestamp::now(),
                payload_size: 64,
            }).unwrap();
        }
    }
    
    {
        let mut w = writer.lock().unwrap();
        drop(w.take());
    }
    
    let files: Vec<_> = std::fs::read_dir(&dir).unwrap()
        .filter_map(|e| e.ok())
        .map(|e| (e.path(), e.metadata().unwrap().len()))
        .collect();
    for (p, s) in &files {
        println!("{:?}: {} bytes", p, s);
    }
}
