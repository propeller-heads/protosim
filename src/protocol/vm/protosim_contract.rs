// TODO: remove skip for clippy dead_code check
#![allow(dead_code)]

use chrono::Utc;
use ethers::{
    abi::{decode, encode, Abi, ParamType, Token},
    core::types::U256,
    prelude::*,
};
use revm::{
    db::DatabaseRef,
    primitives::{alloy_primitives::Keccak256, Address},
};
use std::collections::HashMap;
use tracing::warn;

use crate::{
    evm::simulation::{SimulationEngine, SimulationParameters, SimulationResult},
    protocol::vm::{
        errors::ProtosimError,
        utils::{load_swap_abi, maybe_coerce_error},
    },
};

#[derive(Debug)]
pub struct ProtoSimResponse {
    pub return_value: Vec<Token>,
    pub simulation_result: SimulationResult,
}

pub struct ProtoSimContract<D: DatabaseRef + std::clone::Clone> {
    abi: Abi,
    address: Address,
    engine: SimulationEngine<D>,
}

impl<D: DatabaseRef + std::clone::Clone> ProtoSimContract<D>
where
    D::Error: std::fmt::Debug,
{
    pub fn new(address: Address, engine: SimulationEngine<D>) -> Result<Self, ProtosimError> {
        let abi = load_swap_abi()?;
        Ok(Self { address, abi, engine })
    }
    fn encode_input(&self, fname: &str, args: Vec<Token>) -> Result<Vec<u8>, ProtosimError> {
        let function = self
            .abi
            .functions
            .get(fname)
            .and_then(|funcs| funcs.first())
            .ok_or_else(|| {
                ProtosimError::EncodingError(format!(
                    "Function name {} not found in the ABI",
                    fname
                ))
            })?;

        if function.inputs.len() != args.len() {
            return Err(ProtosimError::EncodingError("Invalid argument count".to_string()));
        }

        let input_types: String = function
            .inputs
            .iter()
            .map(|input| input.kind.to_string())
            .collect::<Vec<_>>()
            .join(",");

        let selector = {
            let mut hasher = Keccak256::new();
            hasher.update(format!("{}({})", fname, input_types));
            let result = hasher.finalize();
            result[..4].to_vec()
        };

        let encoded = encode(&args);
        let mut result = Vec::with_capacity(4 + encoded.len());
        result.extend_from_slice(&selector);
        result.extend(encoded);

        Ok(result)
    }

    pub fn decode_output(
        &self,
        fname: &str,
        encoded: Vec<u8>,
    ) -> Result<Vec<Token>, ProtosimError> {
        let function = self
            .abi
            .functions
            .get(fname)
            .and_then(|funcs| funcs.first())
            .ok_or_else(|| {
                ProtosimError::DecodingError(format!(
                    "Function name {} not found in the ABI",
                    fname
                ))
            })?;

        let output_types: Vec<ParamType> = function
            .outputs
            .iter()
            .map(|output| output.kind.clone())
            .collect();
        let decoded_tokens = decode(&output_types, &encoded).map_err(|e| {
            ProtosimError::DecodingError(format!("Failed to decode output: {:?}", e))
        })?;

        Ok(decoded_tokens)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn call(
        &self,
        fname: &str,
        args: Vec<Token>,
        block_number: u64,
        timestamp: Option<u64>,
        overrides: Option<HashMap<Address, HashMap<U256, U256>>>,
        caller: Option<Address>,
        value: U256,
    ) -> Result<ProtoSimResponse, ProtosimError> {
        let call_data = self.encode_input(fname, args)?;
        let params = SimulationParameters {
            data: Bytes::from(call_data),
            to: self.address,
            block_number,
            timestamp: timestamp.unwrap_or_else(|| {
                Utc::now()
                    .naive_utc()
                    .and_utc()
                    .timestamp() as u64
            }),
            overrides,
            caller: caller.unwrap_or(Address::ZERO),
            value,
            gas_limit: None,
        };

        let sim_result = self.simulate(params)?;

        let output = self
            .decode_output(fname, sim_result.result.to_vec())
            .unwrap_or_else(|err| {
                warn!("Failed to decode output: {:?}", err);
                Vec::new() // Set to empty if decoding fails
            });

        Ok(ProtoSimResponse { return_value: output, simulation_result: sim_result })
    }

    fn simulate(&self, params: SimulationParameters) -> Result<SimulationResult, ProtosimError> {
        self.engine
            .simulate(&params)
            .map_err(|e| {
                if let Err(coerced_err) = maybe_coerce_error(e, "pool_state", params.gas_limit) {
                    ProtosimError::SimulationFailure(coerced_err.to_string())
                } else {
                    ProtosimError::SimulationFailure("Unknown simulation error".to_string())
                }
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use revm::primitives::{hex, AccountInfo, Address, Bytecode, B256, U256 as rU256};
    use std::str::FromStr;

    #[derive(Debug, Clone)]
    struct MockDatabase;

    impl DatabaseRef for MockDatabase {
        type Error = String;

        fn basic_ref(
            &self,
            _address: revm::precompile::Address,
        ) -> Result<Option<AccountInfo>, Self::Error> {
            Ok(Some(AccountInfo::default()))
        }

        fn code_by_hash_ref(&self, _code_hash: B256) -> Result<Bytecode, Self::Error> {
            Ok(Bytecode::new())
        }

        fn storage_ref(
            &self,
            _address: revm::precompile::Address,
            _index: rU256,
        ) -> Result<rU256, Self::Error> {
            Ok(rU256::from(0))
        }

        fn block_hash_ref(&self, _number: u64) -> Result<B256, Self::Error> {
            Ok(B256::default())
        }
    }

    fn create_mock_engine() -> SimulationEngine<MockDatabase> {
        SimulationEngine::new(MockDatabase, false)
    }

    fn create_contract() -> ProtoSimContract<MockDatabase> {
        let address = Address::ZERO;
        let engine = create_mock_engine();
        ProtoSimContract::new(address, engine).unwrap()
    }

    #[test]
    fn test_encode_input_get_capabilities() {
        let contract = create_contract();

        // Arguments for the 'getCapabilities' function
        let pool_id =
            "1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef".to_string();
        let sell_token = "0000000000000000000000000000000000000002".to_string();
        let buy_token = "0000000000000000000000000000000000000003".to_string();

        let encoded_input = contract.encode_input(
            "getCapabilities",
            vec![
                Token::FixedBytes(hex::decode(pool_id.clone()).unwrap()),
                Token::Address(H160::from_str(&sell_token).unwrap()),
                Token::Address(H160::from_str(&buy_token).unwrap()),
            ],
        );

        assert!(encoded_input.is_ok());
        let encoded_result = encoded_input.unwrap();

        // The expected selector for "getCapabilities(bytes32,address,address)"
        let expected_selector = hex!("48bd7dfd");
        assert_eq!(&encoded_result[..4], &expected_selector[..]);

        let expected_pool_id =
            hex!("1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef");
        let expected_sell_token =
            hex!("0000000000000000000000000000000000000000000000000000000000000002"); // padded to 32 bytes
        let expected_buy_token =
            hex!("0000000000000000000000000000000000000000000000000000000000000003"); // padded to 32 bytes

        assert_eq!(&encoded_result[4..36], &expected_pool_id); // 32 bytes for poolId
        assert_eq!(&encoded_result[36..68], &expected_sell_token); // 32 bytes for address (padded)
        assert_eq!(&encoded_result[68..100], &expected_buy_token); // 32 bytes for address (padded)
    }

    #[test]
    fn test_decode_output_get_tokens() {
        let contract = create_contract();

        let token_1 = H160::from_str("0000000000000000000000000000000000000002").unwrap();
        let token_2 = H160::from_str("0000000000000000000000000000000000000003").unwrap();

        let encoded_output = hex!("
        0000000000000000000000000000000000000000000000000000000000000020" // Offset to the start of the array
        "0000000000000000000000000000000000000000000000000000000000000002" // Array length: 2
        "0000000000000000000000000000000000000000000000000000000000000002" // Token 1
        "0000000000000000000000000000000000000000000000000000000000000003" // Token 2
        );

        let decoded = contract
            .decode_output("getTokens", encoded_output.to_vec())
            .unwrap();

        let expected_tokens =
            vec![Token::Array(vec![Token::Address(token_1), Token::Address(token_2)])];
        assert_eq!(decoded, expected_tokens);
    }
}
