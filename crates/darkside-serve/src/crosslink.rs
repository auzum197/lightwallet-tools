//! The Crosslink variant's server: the macro plus the overlay's additive
//! RPCs (roster, bond info, faucet).

use lightwallet_proto_crosslink as proto;

const FAUCET_ZATS: u64 = 100_000_000;

darkside_streamer!(proto, extra_rpcs {
    async fn get_roster(
        &self,
        _request: tonic::Request<proto::Empty>,
    ) -> Result<tonic::Response<proto::Bytes>, tonic::Status> {
        tracing::info!("GetRoster");
        // No BFT process exists here; an empty roster is the truthful
        // answer for a synthetic featurenet.
        Ok(tonic::Response::new(proto::Bytes { data: Vec::new() }))
    }

    async fn get_bond_info(
        &self,
        request: tonic::Request<proto::BondInfoRequest>,
    ) -> Result<tonic::Response<proto::BondInfoResponse>, tonic::Status> {
        let req = request.into_inner();
        match tier() {
            Tier::Trace => tracing::trace!(?req, "GetBondInfo"),
            _ => tracing::info!("GetBondInfo"),
        }
        let response = proto::BondInfoResponse {
            amount: 0,
            status: 0,
        };
        tracing::info!(amount = response.amount, status = response.status, "GetBondInfo ->");
        Ok(tonic::Response::new(response))
    }

    async fn request_faucet_donation(
        &self,
        request: tonic::Request<proto::FaucetRequest>,
    ) -> Result<tonic::Response<proto::FaucetResponse>, tonic::Status> {
        // A faucet is a fund to the requesting address, scheduled for the
        // next block; the live driver's tick mines it.
        let faucet = request.into_inner();
        match tier() {
            Tier::Trace => tracing::trace!(?faucet, "RequestFaucetDonation"),
            _ => tracing::info!(address = %faucet.address, "RequestFaucetDonation"),
        }
        let address = faucet.address;
        self.emu
            .with_chain_mut(|chain| {
                let at = chain.tip_height() + 1;
                chain.fund(darkside_chain::FundSpec {
                    recipient: darkside_chain::Recipient::Literal(address),
                    pool: None,
                    zats: FAUCET_ZATS,
                    outputs: 1,
                    at,
                    via_coinbase: false,
                    corruption: None,
                })
            })
            .map_err(|e| tonic::Status::invalid_argument(e.to_string()))?;
        tracing::info!(amount = FAUCET_ZATS, "RequestFaucetDonation ->");
        Ok(tonic::Response::new(proto::FaucetResponse {
            amount: FAUCET_ZATS,
        }))
    }
});
