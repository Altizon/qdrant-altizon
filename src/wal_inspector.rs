use std::env;
use std::path::Path;

use collection::operations::CollectionUpdateOperations;
use collection::wal::SerdeWal;
use storage::content_manager::consensus::consensus_wal::ConsensusOpWal;
use wal::WalOptions;

/// Executable to inspect the content of a write ahead log folder (collection OR consensus WAL).
/// e.g `cargo run --bin wal_inspector storage/collections/test-collection/0/wal/ collection`
/// e.g `cargo run --bin wal_inspector -- storage/node4/wal/ consensus`
fn main() {
    let args: Vec<String> = env::args().collect();
    let wal_path = Path::new(&args[1]);
    let wal_type = args[2].as_str();
    match wal_type {
        "collection" => print_collection_wal(wal_path),
        "consensus" => print_consensus_wal(wal_path),
        _ => eprintln!("Unknown wal type: {}", wal_type),
    }
}

fn print_consensus_wal(wal_path: &Path) {
    // must live within a folder named `collections_meta_wal`
    let wal = ConsensusOpWal::new(wal_path.to_str().unwrap());
    println!("==========================");
    let first_index = wal.first_entry().unwrap();
    println!("First entry: {:?}", first_index);
    let last_index = wal.last_entry().unwrap();
    println!("Last entry: {:?}", last_index);
    println!("Offset of first entry: {:?}", wal.index_offset().unwrap());
    let entries = wal
        .entries(
            first_index.map(|f| f.index).unwrap_or(1),
            last_index.map(|f| f.index).unwrap_or(1),
            None,
        )
        .unwrap();
    for entry in entries {
        println!("==========================");
        let data = entry.data;
        println!(
            "Entry ID:{} term:{} entry_type:{} data:{:?}",
            entry.index, entry.term, entry.entry_type, data
        );
    }
}

fn print_collection_wal(wal_path: &Path) {
    let wal: Result<SerdeWal<CollectionUpdateOperations>, _> =
        SerdeWal::new(wal_path.to_str().unwrap(), WalOptions::default());

    match wal {
        Err(error) => {
            eprintln!("Unable to open write ahead log in directory {wal_path:?}: {error}.");
        }
        Ok(wal) => {
            // print all entries
            let mut count = 0;
            for (idx, op) in wal.read_all() {
                println!("==========================");
                println!("Entry {}", idx);
                println!("{:?}", op);
                count += 1;
            }
            println!("==========================");
            println!("End of WAL.");
            println!("Found {} entries.", count);
        }
    }
}
