//! Mock endpoint for the CROSSLINK variant: the shared surface plus the
//! overlay's additive RPCs (roster, bond info, faucet).

use lightwallet_proto_crosslink as proto;

crate::mock::mock_streamer!(
    proto,
    extra_state {
        roster: Vec<u8>,
        bonds: std::collections::HashMap<Vec<u8>, proto::BondInfoResponse>,
        faucet_amount: u64,
    },
    extra_config {
        /// Roster bytes served by `GetRoster`.
        pub fn with_roster(mut self, roster: Vec<u8>) -> Self {
            self.roster = roster;
            self
        }

        /// Register a bond for `GetBondInfo`; unknown keys get `NOT_FOUND`.
        pub fn with_bond(mut self, bond_key: Vec<u8>, info: proto::BondInfoResponse) -> Self {
            self.bonds.insert(bond_key, info);
            self
        }

        /// Zatoshis granted per `RequestFaucetDonation`.
        pub fn with_faucet_amount(mut self, zatoshis: u64) -> Self {
            self.faucet_amount = zatoshis;
            self
        }
    },
    extra_rpcs {
        async fn get_roster(
            &self,
            _request: tonic::Request<proto::Empty>,
        ) -> std::result::Result<tonic::Response<proto::Bytes>, tonic::Status> {
            self.check(crate::Rpc::GetRoster)?;
            Ok(tonic::Response::new(proto::Bytes { data: self.roster.clone() }))
        }

        async fn get_bond_info(
            &self,
            request: tonic::Request<proto::BondInfoRequest>,
        ) -> std::result::Result<tonic::Response<proto::BondInfoResponse>, tonic::Status> {
            self.check(crate::Rpc::GetBondInfo)?;
            self.bonds
                .get(&request.into_inner().bond_key)
                .cloned()
                .map(tonic::Response::new)
                .ok_or_else(|| tonic::Status::not_found("no such bond"))
        }

        async fn request_faucet_donation(
            &self,
            _request: tonic::Request<proto::FaucetRequest>,
        ) -> std::result::Result<tonic::Response<proto::FaucetResponse>, tonic::Status> {
            self.check(crate::Rpc::RequestFaucetDonation)?;
            Ok(tonic::Response::new(proto::FaucetResponse {
                amount: self.faucet_amount,
            }))
        }
    },
);
