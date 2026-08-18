//! Request-message builders shared by both wire surfaces. `block_range` is
//! used by the indexer methods (`indexer_methods.rs`); `taddr_filter` wraps it
//! with an address for the identity methods (`identity.rs`). Kept apart from
//! either caller so neither has to reach into the other for a helper.

macro_rules! block_range {
    ($proto:ident, $start:expr, $end:expr) => {
        $proto::BlockRange {
            start: Some($proto::BlockId {
                height: $start,
                hash: Vec::new(),
            }),
            end: Some($proto::BlockId {
                height: $end,
                hash: Vec::new(),
            }),
            pool_types: Vec::new(),
        }
    };
}

macro_rules! taddr_filter {
    ($proto:ident, $address:expr, $start:expr, $end:expr) => {
        $proto::TransparentAddressBlockFilter {
            address: $address,
            range: Some($crate::request::block_range!($proto, $start, $end)),
        }
    };
}

pub(crate) use {block_range, taddr_filter};
