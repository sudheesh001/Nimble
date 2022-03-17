use crate::endorser_state::EndorserState;
use crate::errors::EndorserError;
use clap::{App, Arg};
use ledger::{
  signature::{PublicKeyTrait, SignatureTrait},
  NimbleDigest,
};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use tonic::transport::Server;
use tonic::{Request, Response, Status};

mod endorser_state;
mod errors;

pub mod endorser_proto {
  tonic::include_proto!("endorser_proto");
}

use endorser_proto::endorser_call_server::{EndorserCall, EndorserCallServer};
use endorser_proto::{
  AppendReq, AppendResp, AppendViewLedgerReq, AppendViewLedgerResp, GetPublicKeyReq,
  GetPublicKeyResp, InitializeStateReq, InitializeStateResp, LedgerTailMapEntry, NewLedgerReq,
  NewLedgerResp, ReadLatestReq, ReadLatestResp, ReadLatestStateReq, ReadLatestStateResp,
  ReadLatestViewLedgerReq, ReadLatestViewLedgerResp,
};

#[derive(Default, Copy, Debug, Clone)]
pub struct EndorserFlags {
  is_locked: bool,
}

impl EndorserFlags {
  pub fn new() -> Self {
    EndorserFlags { is_locked: false }
  }

  pub fn set_lock(&mut self, to_lock: bool) {
    self.is_locked = to_lock;
  }

  pub fn get_lock(&self) -> bool {
    self.is_locked
  }
}

pub struct EndorserServiceState {
  state: Arc<RwLock<EndorserState>>,
  flags: Arc<RwLock<EndorserFlags>>,
}

impl EndorserServiceState {
  pub fn new() -> Self {
    EndorserServiceState {
      state: Arc::new(RwLock::new(EndorserState::new())),
      flags: Arc::new(RwLock::new(EndorserFlags::new())),
    }
  }
}

impl Default for EndorserServiceState {
  fn default() -> Self {
    Self::new()
  }
}

#[tonic::async_trait]
impl EndorserCall for EndorserServiceState {
  async fn get_public_key(
    &self,
    _req: Request<GetPublicKeyReq>,
  ) -> Result<Response<GetPublicKeyResp>, Status> {
    let pk = self
      .state
      .read()
      .expect("Failed to acquire read lock")
      .get_public_key();

    let reply = GetPublicKeyResp {
      pk: pk.to_bytes().to_vec(),
    };

    Ok(Response::new(reply))
  }

  async fn new_ledger(
    &self,
    req: Request<NewLedgerReq>,
  ) -> Result<Response<NewLedgerResp>, Status> {
    let NewLedgerReq { handle } = req.into_inner();
    let handle = {
      let handle_instance = NimbleDigest::from_bytes(&handle);
      if handle_instance.is_err() {
        return Err(Status::invalid_argument("Handle size is invalid"));
      }
      handle_instance.unwrap()
    };

    let mut endorser = self
      .state
      .write()
      .expect("Unable to get a write lock on EndorserState");

    let signature = endorser
      .new_ledger(&handle)
      .expect("Unable to get the signature on genesis handle");

    let reply = NewLedgerResp {
      signature: signature.to_bytes().to_vec(),
    };
    Ok(Response::new(reply))
  }

  async fn append(&self, req: Request<AppendReq>) -> Result<Response<AppendResp>, Status> {
    let AppendReq {
      handle,
      block_hash,
      cond_updated_tail_hash,
      cond_updated_tail_height,
    } = req.into_inner();

    let handle_instance = NimbleDigest::from_bytes(&handle);
    let block_hash_instance = NimbleDigest::from_bytes(&block_hash);
    let cond_updated_tail_hash_instance = NimbleDigest::from_bytes(&cond_updated_tail_hash);

    if handle_instance.is_err()
      || block_hash_instance.is_err()
      || cond_updated_tail_hash_instance.is_err()
    {
      return Err(Status::invalid_argument("Invalid input sizes"));
    }

    let mut endorser_state = self.state.write().expect("Unable to obtain write lock");
    let res = endorser_state.append(
      &handle_instance.unwrap(),
      &block_hash_instance.unwrap(),
      &cond_updated_tail_hash_instance.unwrap(),
      cond_updated_tail_height as usize,
    );

    match res {
      Ok(signature) => {
        let reply = AppendResp {
          signature: signature.to_bytes().to_vec(),
        };
        Ok(Response::new(reply))
      },

      Err(error) => match error {
        EndorserError::OutOfOrderAppend => Err(Status::failed_precondition("Out of order append")),
        EndorserError::InvalidConditionalTail => {
          Err(Status::aborted("Invalid conditional tail hash"))
        },
        EndorserError::InvalidLedgerName => Err(Status::not_found("Ledger handle not found")),
        EndorserError::LedgerHeightOverflow => Err(Status::out_of_range("Ledger height overflow")),
        EndorserError::InvalidTailHeight => Err(Status::already_exists("Invalid ledgher height")),
        _ => Err(Status::aborted("Failed to append")),
      },
    }
  }

  async fn read_latest(
    &self,
    request: Request<ReadLatestReq>,
  ) -> Result<Response<ReadLatestResp>, Status> {
    let ReadLatestReq { handle, nonce } = request.into_inner();
    let handle = {
      let res = NimbleDigest::from_bytes(&handle);
      if res.is_err() {
        return Err(Status::invalid_argument("Invalid handle size"));
      }
      res.unwrap()
    };
    let latest_state = self.state.read().expect("Failed to acquire read lock");
    let res = latest_state.read_latest(&handle, &nonce);

    match res {
      Ok(signature) => {
        let reply = ReadLatestResp {
          signature: signature.to_bytes().to_vec(),
        };
        Ok(Response::new(reply))
      },
      Err(_) => Err(Status::aborted("Failed to process read_latest")),
    }
  }

  async fn append_view_ledger(
    &self,
    req: Request<AppendViewLedgerReq>,
  ) -> Result<Response<AppendViewLedgerResp>, Status> {
    let AppendViewLedgerReq {
      block_hash,
      cond_updated_tail_hash,
    } = req.into_inner();

    let block_hash_instance = NimbleDigest::from_bytes(&block_hash);
    let cond_updated_tail_hash_instance = NimbleDigest::from_bytes(&cond_updated_tail_hash);

    if block_hash_instance.is_err() || cond_updated_tail_hash_instance.is_err() {
      return Err(Status::invalid_argument("Invalid input sizes"));
    }

    let mut endorser_state = self.state.write().expect("Unable to obtain write lock");
    let res = endorser_state.append_view_ledger(
      &block_hash_instance.unwrap(),
      &cond_updated_tail_hash_instance.unwrap(),
    );

    match res {
      Ok(signature) => {
        let reply = AppendViewLedgerResp {
          signature: signature.to_bytes().to_vec(),
        };
        Ok(Response::new(reply))
      },

      Err(_) => Err(Status::aborted("Failed to append_view_ledger")),
    }
  }

  async fn read_latest_view_ledger(
    &self,
    request: Request<ReadLatestViewLedgerReq>,
  ) -> Result<Response<ReadLatestViewLedgerResp>, Status> {
    let ReadLatestViewLedgerReq { nonce } = request.into_inner();
    let endorser = self.state.read().expect("Failed to acquire read lock");
    let res = endorser.read_latest_view_ledger(&nonce);

    match res {
      Ok(signature) => {
        let reply = ReadLatestViewLedgerResp {
          signature: signature.to_bytes().to_vec(),
        };
        Ok(Response::new(reply))
      },
      Err(_) => Err(Status::aborted("Failed to process read_latest_view_ledger")),
    }
  }

  async fn initialize_state(
    &self,
    req: Request<InitializeStateReq>,
  ) -> Result<Response<InitializeStateResp>, Status> {
    let InitializeStateReq {
      ledger_tail_map,
      view_ledger_tail,
      view_ledger_height,
      block_hash,
      cond_updated_tail_hash,
    } = req.into_inner();
    let ledger_tail_map_rs: HashMap<NimbleDigest, (NimbleDigest, usize)> = ledger_tail_map
      .into_iter()
      .map(|e| {
        (
          NimbleDigest::from_bytes(&e.handle).unwrap(),
          (
            NimbleDigest::from_bytes(&e.tail).unwrap(),
            e.height as usize,
          ),
        )
      })
      .collect();
    let view_ledger_tail_rs = (
      NimbleDigest::from_bytes(&view_ledger_tail).unwrap(),
      view_ledger_height as usize,
    );
    let block_hash_rs = NimbleDigest::from_bytes(&block_hash).unwrap();
    let cond_updated_tail_hash_rs = NimbleDigest::from_bytes(&cond_updated_tail_hash).unwrap();
    let res = self
      .state
      .write()
      .expect("Failed to acquire write lock")
      .initialize_state(
        &ledger_tail_map_rs,
        &view_ledger_tail_rs,
        &block_hash_rs,
        &cond_updated_tail_hash_rs,
      );

    match res {
      Ok(signature) => {
        let reply = InitializeStateResp {
          signature: signature.to_bytes().to_vec(),
        };
        Ok(Response::new(reply))
      },
      Err(_) => Err(Status::aborted("Failed to endorse_state")),
    }
  }

  async fn read_latest_state(
    &self,
    request: Request<ReadLatestStateReq>,
  ) -> Result<Response<ReadLatestStateResp>, Status> {
    let ReadLatestStateReq {
      nonce,
      view_ledger_height,
      to_lock,
    } = request.into_inner();

    let res = self
      .state
      .read()
      .expect("Failed to acquire read lock")
      .read_latest_state(&nonce);

    match res {
      Ok((ledger_view, signature)) => {
        if to_lock && view_ledger_height == ledger_view.view_tail_metablock.get_height() as u64 {
          self
            .flags
            .write()
            .expect("Failed to acquire write lock")
            .set_lock(to_lock);
        }
        let ledger_tail_map: Vec<LedgerTailMapEntry> = ledger_view
          .ledger_tail_map
          .iter()
          .map(|(handle, (tail, height))| LedgerTailMapEntry {
            handle: handle.to_bytes(),
            tail: tail.to_bytes(),
            height: *height as u64,
          })
          .collect();
        let reply = ReadLatestStateResp {
          ledger_tail_map,
          view_ledger_tail_prev: ledger_view
            .view_tail_metablock
            .get_prev()
            .to_bytes()
            .to_vec(),
          view_ledger_tail_view: ledger_view
            .view_tail_metablock
            .get_block_hash()
            .to_bytes()
            .to_vec(),
          view_ledger_tail_height: ledger_view.view_tail_metablock.get_height() as u64,
          is_locked: self
            .flags
            .read()
            .expect("Failed to acquire read lock")
            .get_lock(),
          signature: signature.to_bytes().to_vec(),
        };
        Ok(Response::new(reply))
      },
      Err(_) => Err(Status::aborted("Failed to read latest state")),
    }
  }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
  let config = App::new("endorser")
    .arg(
      Arg::with_name("host")
        .help("The hostname to run the Service On. Default: [::1]")
        .default_value("[::1]")
        .index(2),
    )
    .arg(
      Arg::with_name("port")
        .help("The port number to run the Service On. Default: 9096")
        .default_value("9090")
        .index(1),
    );
  let cli_matches = config.get_matches();
  let hostname = cli_matches.value_of("host").unwrap();
  let port_number = cli_matches.value_of("port").unwrap();
  let addr = format!("{}:{}", hostname, port_number).parse()?;
  let server = EndorserServiceState::new();

  println!("Endorser host listening on {:?}", addr);

  Server::builder()
    .add_service(EndorserCallServer::new(server))
    .serve(addr)
    .await?;

  Ok(())
}
