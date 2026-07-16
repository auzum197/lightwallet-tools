//! Mock endpoint for the CANONICAL variant.

use lightwallet_proto_canonical as proto;

crate::mock::mock_streamer!(proto, extra_state {}, extra_config {}, extra_rpcs {},);
