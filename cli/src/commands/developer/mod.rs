// Copyright (C) 2019-2023 Aleo Systems Inc.
// This file is part of the snarkOS library.

// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at:
// http://www.apache.org/licenses/LICENSE-2.0

// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

mod decrypt;
pub use decrypt::*;

mod deploy;
pub use deploy::*;

mod execute;
pub use execute::*;

mod scan;
pub use scan::*;

mod transfer_private;
pub use transfer_private::*;

use snarkvm::{
    package::Package,
    prelude::{
        block::Transaction,
        Address,
        Ciphertext,
        Identifier,
        Literal,
        Plaintext,
        PrivateKey,
        Program,
        ProgramID,
        Record,
        ToBytes,
        Value,
        ViewKey,
    },
};

use anyhow::{bail, ensure, Result};
use clap::Parser;
use colored::Colorize;
use std::{path::PathBuf, str::FromStr};

type CurrentAleo = snarkvm::circuit::AleoV0;
type CurrentNetwork = snarkvm::prelude::Testnet3;

/// Commands to deploy and execute transactions
#[derive(Debug, Parser)]
pub enum Developer {
    /// Decrypt a ciphertext.
    Decrypt(Decrypt),
    /// Deploy a program.
    Deploy(Deploy),
    /// Execute a program function.
    Execute(Execute),
    /// Scan the node for records.
    Scan(Scan),
    /// Execute the `credits.aleo/transfer_private` function.
    TransferPrivate(TransferPrivate),
}

impl Developer {
    pub fn parse(self) -> Result<String> {
        match self {
            Self::Decrypt(decrypt) => decrypt.parse(),
            Self::Deploy(deploy) => deploy.parse(),
            Self::Execute(execute) => execute.parse(),
            Self::Scan(scan) => scan.parse(),
            Self::TransferPrivate(transfer_private) => transfer_private.parse(),
        }
    }

    /// Parse the package from the directory.
    fn parse_package(program_id: ProgramID<CurrentNetwork>, path: Option<String>) -> Result<Package<CurrentNetwork>> {
        // Instantiate a path to the directory containing the manifest file.
        let directory = match path {
            Some(path) => PathBuf::from_str(&path)?,
            None => std::env::current_dir()?,
        };

        // Load the package.
        let package = Package::open(&directory)?;

        ensure!(
            package.program_id() == &program_id,
            "The program name in the package does not match the specified program name"
        );

        // Return the package.
        Ok(package)
    }

    /// Parses the record string. If the string is a ciphertext, then attempt to decrypt it.
    fn parse_record(
        private_key: &PrivateKey<CurrentNetwork>,
        record: &str,
    ) -> Result<Record<CurrentNetwork, Plaintext<CurrentNetwork>>> {
        match record.starts_with("record1") {
            true => {
                // Parse the ciphertext.
                let ciphertext = Record::<CurrentNetwork, Ciphertext<CurrentNetwork>>::from_str(record)?;
                // Derive the view key.
                let view_key = ViewKey::try_from(private_key)?;
                // Decrypt the ciphertext.
                ciphertext.decrypt(&view_key)
            }
            false => Record::<CurrentNetwork, Plaintext<CurrentNetwork>>::from_str(record),
        }
    }

    /// Fetch the program from the given endpoint.
    fn fetch_program(program_id: &ProgramID<CurrentNetwork>, endpoint: &str) -> Result<Program<CurrentNetwork>> {
        // Send a request to the query node.
        let response = ureq::get(&format!("{endpoint}/testnet3/program/{program_id}")).call();

        // Deserialize the program.
        match response {
            Ok(response) => response.into_json().map_err(|err| err.into()),
            Err(err) => match err {
                ureq::Error::Status(_status, response) => {
                    bail!(response.into_string().unwrap_or("Response too large!".to_owned()))
                }
                err => bail!(err),
            },
        }
    }

    /// Fetch the public balance in microcredits associated with the address from the given endpoint.
    fn get_public_balance(address: &Address<CurrentNetwork>, endpoint: &str) -> Result<u64> {
        // Initialize the program id and account identifier.
        let credits = ProgramID::<CurrentNetwork>::from_str("credits.aleo")?;
        let account_mapping = Identifier::<CurrentNetwork>::from_str("account")?;

        // Send a request to the query node.
        let response =
            ureq::get(&format!("{endpoint}/testnet3/program/{credits}/mapping/{account_mapping}/{address}")).call();

        // Deserialize the balance.
        let balance: Result<Option<Value<CurrentNetwork>>> = match response {
            Ok(response) => response.into_json().map_err(|err| err.into()),
            Err(err) => match err {
                ureq::Error::Status(_status, response) => {
                    bail!(response.into_string().unwrap_or("Response too large!".to_owned()))
                }
                err => bail!(err),
            },
        };

        // Return the balance in microcredits.
        match balance {
            Ok(Some(Value::Plaintext(Plaintext::Literal(Literal::<CurrentNetwork>::U64(amount), _)))) => Ok(*amount),
            Ok(None) => Ok(0),
            Ok(Some(..)) => bail!("Failed to deserialize balance for {address}"),
            Err(err) => bail!("Failed to fetch balance for {address}: {err}"),
        }
    }

    /// Determine if the transaction should be broadcast or displayed to user.
    fn handle_transaction(
        broadcast: Option<String>,
        dry_run: bool,
        store: Option<String>,
        transaction: Transaction<CurrentNetwork>,
        operation: String,
    ) -> Result<String> {
        // Get the transaction id.
        let transaction_id = transaction.id();

        // Ensure the transaction is not a fee transaction.
        ensure!(!transaction.is_fee(), "The transaction is a fee transaction and cannot be broadcast");

        // Determine if the transaction should be stored.
        if let Some(path) = store {
            match PathBuf::from_str(&path) {
                Ok(file_path) => {
                    let transaction_bytes = transaction.to_bytes_le()?;
                    std::fs::write(&file_path, transaction_bytes)?;
                    println!("Transaction {transaction_id} was stored to {}", file_path.display());
                }
                Err(err) => {
                    println!("The transaction was unable to be stored due to: {err}");
                }
            }
        };

        // Determine if the transaction should be broadcast to the network.
        if let Some(endpoint) = broadcast {
            // Send the deployment request to the local development node.
            match ureq::post(&endpoint).send_json(&transaction) {
                Ok(id) => {
                    // Remove the quotes from the response.
                    let response_string = id.into_string()?.trim_matches('\"').to_string();
                    ensure!(
                        response_string == transaction_id.to_string(),
                        "The response does not match the transaction id. ({response_string} != {transaction_id})"
                    );

                    match transaction {
                        Transaction::Deploy(..) => {
                            println!(
                                "✅ Successfully broadcast deployment {transaction_id} ('{}') to {}.",
                                operation.bold(),
                                endpoint
                            )
                        }
                        Transaction::Execute(..) => {
                            println!(
                                "✅ Successfully broadcast execution {transaction_id} ('{}') to {}.",
                                operation.bold(),
                                endpoint
                            )
                        }
                        Transaction::Fee(..) => {
                            println!("❌ Failed to broadcast fee '{}' to the {}.", operation.bold(), endpoint)
                        }
                    }
                }
                Err(error) => {
                    let error_message = match error {
                        ureq::Error::Status(code, response) => {
                            format!("(status code {code}: {:?})", response.into_string()?)
                        }
                        ureq::Error::Transport(err) => format!("({err})"),
                    };

                    match transaction {
                        Transaction::Deploy(..) => {
                            bail!("❌ Failed to deploy '{}' to {}: {}", operation.bold(), &endpoint, error_message)
                        }
                        Transaction::Execute(..) => {
                            bail!(
                                "❌ Failed to broadcast execution '{}' to {}: {}",
                                operation.bold(),
                                &endpoint,
                                error_message
                            )
                        }
                        Transaction::Fee(..) => {
                            bail!(
                                "❌ Failed to broadcast fee '{}' to {}: {}",
                                operation.bold(),
                                &endpoint,
                                error_message
                            )
                        }
                    }
                }
            };

            // Output the transaction id.
            Ok(transaction_id.to_string())
        } else if dry_run {
            // Output the transaction string.
            Ok(transaction.to_string())
        } else {
            Ok("".to_string())
        }
    }
}
