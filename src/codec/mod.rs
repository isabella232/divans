// Copyright 2017 Dropbox, Inc
//
//   Licensed under the Apache License, Version 2.0 (the "License");
//   you may not use this file except in compliance with the License.
//   You may obtain a copy of the License at
//
//       http://www.apache.org/licenses/LICENSE-2.0
//
//   Unless required by applicable law or agreed to in writing, software
//   distributed under the License is distributed on an "AS IS" BASIS,
//   WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
//   See the License for the specific language governing permissions and
//   limitations under the License.

#![allow(dead_code)]
use core;
use alloc::{SliceWrapper, Allocator};
use brotli::BrotliResult;
pub const CMD_BUFFER_SIZE: usize = 16;
use super::interface::{
    BillingDesignation,
    CrossCommandBilling,
    BlockSwitch,
    LiteralBlockSwitch,
    Nop
};
pub mod weights;
mod interface;
pub use self::interface::{
    EncoderOrDecoderSpecialization,
    AllocatedMemoryPrefix,
    CrossCommandState,
};

pub mod copy;
pub mod dict;
pub mod literal;
pub mod context_map;
pub mod block_type;
pub mod priors;
#[cfg(feature="billing")]
#[cfg(feature="debug_entropy")]
use priors::summarize_prior_billing;

/*
use std::io::Write;
macro_rules! println_stderr(
    ($($val:tt)*) => { {
        writeln!(&mut ::std::io::stderr(), $($val)*).unwrap();
    } }
);
*/
use super::probability::{CDF2, CDF16, Speed};

//#[cfg(feature="billing")]
//use std::io::Write;
//#[cfg(feature="billing")]
//macro_rules! println_stderr(
//    ($($val:tt)*) => { {
//        writeln!(&mut ::std::io::stderr(), $($val)*).unwrap();
//    } }
//);
//
//#[cfg(not(feature="billing"))]
//macro_rules! println_stderr(
//    ($($val:tt)*) => { {
////        writeln!(&mut ::std::io::stderr(), $($val)*).unwrap();
//    } }
//);
use super::interface::{
    ArithmeticEncoderOrDecoder,
    Command,
    CopyCommand,
    DictCommand,
    LiteralCommand,
    LiteralPredictionModeNibble,
    PredictionModeContextMap,
/*    LITERAL_PREDICTION_MODE_SIGN,
    LITERAL_PREDICTION_MODE_UTF8,
    LITERAL_PREDICTION_MODE_MSB6,
    LITERAL_PREDICTION_MODE_LSB6,
*/
};







enum EncodeOrDecodeState<AllocU8: Allocator<u8> > {
    Begin,
    Literal(literal::LiteralState<AllocU8>),
    Dict(dict::DictState),
    Copy(copy::CopyState),
    BlockSwitchLiteral(block_type::LiteralBlockTypeState),
    BlockSwitchCommand(block_type::BlockTypeState),
    BlockSwitchDistance(block_type::BlockTypeState),
    PredictionMode(context_map::PredictionModeState),
    PopulateRingBuffer(Command<AllocatedMemoryPrefix<AllocU8>>),
    DivansSuccess,
    EncodedShutdownNode, // in flush/close state (encoder only) and finished flushing the EOF node type
    ShutdownCoder,
    CoderBufferDrain,
    WriteChecksum(usize),
}

const CHECKSUM_LENGTH: usize = 8;


impl<AllocU8:Allocator<u8>> Default for EncodeOrDecodeState<AllocU8> {
    fn default() -> Self {
        EncodeOrDecodeState::Begin
    }
}



pub fn command_type_to_nibble<SliceType:SliceWrapper<u8>>(cmd:&Command<SliceType>,
                                                          is_end: bool) -> u8 {

    if is_end {
        return 0xf;
    }
    match *cmd {
        Command::Copy(_) => 0x1,
        Command::Dict(_) => 0x2,
        Command::Literal(_) => 0x3,
        Command::BlockSwitchLiteral(_) => 0x4,
        Command::BlockSwitchCommand(_) => 0x5,
        Command::BlockSwitchDistance(_) => 0x6,
        Command::PredictionMode(_) => 0x7,
    }
}
#[cfg(feature="bitcmdselect")]
fn use_legacy_bitwise_command_type_code() -> bool {
    true
}
fn get_command_state_from_nibble<AllocU8:Allocator<u8>>(command_type_code:u8) -> EncodeOrDecodeState<AllocU8> {
   match command_type_code {
      1 => EncodeOrDecodeState::Copy(copy::CopyState {
                            cc: CopyCommand {
                                distance:0,
                                num_bytes:0,
                            },
                            state:copy::CopySubstate::Begin,
                        }),
      2 => EncodeOrDecodeState::Dict(dict::DictState {
                                dc: DictCommand::nop(),
                                state: dict::DictSubstate::Begin,
                            }),
      3 => EncodeOrDecodeState::Literal(literal::LiteralState {
                                lc:LiteralCommand::<AllocatedMemoryPrefix<AllocU8>>::nop(),
                                state:literal::LiteralSubstate::Begin,
                            }),
     4 => EncodeOrDecodeState::BlockSwitchLiteral(block_type::LiteralBlockTypeState::Begin),
     5 => EncodeOrDecodeState::BlockSwitchCommand(block_type::BlockTypeState::Begin),
     6 => EncodeOrDecodeState::BlockSwitchDistance(block_type::BlockTypeState::Begin),
     7 => EncodeOrDecodeState::PredictionMode(context_map::PredictionModeState::Begin),
     0xf => EncodeOrDecodeState::DivansSuccess,
      _ => panic!("unimpl"),
   }
}

pub struct DivansCodec<ArithmeticCoder:ArithmeticEncoderOrDecoder,
                       Specialization:EncoderOrDecoderSpecialization,
                       Cdf16:CDF16,
                       AllocU8: Allocator<u8>,
                       AllocCDF2:Allocator<CDF2>,
                       AllocCDF16:Allocator<Cdf16>> {
    cross_command_state: CrossCommandState<ArithmeticCoder,
                                           Specialization,
                                           Cdf16,
                                           AllocU8,
                                           AllocCDF2,
                                           AllocCDF16>,
    state : EncodeOrDecodeState<AllocU8>,
}

pub enum OneCommandReturn {
    Advance,
    BufferExhausted(BrotliResult),
}
impl<ArithmeticCoder:ArithmeticEncoderOrDecoder,
     Specialization: EncoderOrDecoderSpecialization,
     Cdf16:CDF16,
     AllocU8: Allocator<u8>,
     AllocCDF2: Allocator<CDF2>,
     AllocCDF16:Allocator<Cdf16>> DivansCodec<ArithmeticCoder, Specialization, Cdf16, AllocU8, AllocCDF2, AllocCDF16> {
    pub fn free(self) -> (AllocU8, AllocCDF2, AllocCDF16) {
        self.cross_command_state.free()
    }
    pub fn new(m8:AllocU8,
               mcdf2:AllocCDF2,
               mcdf16:AllocCDF16,
               coder: ArithmeticCoder,
               specialization: Specialization,
               ring_buffer_size: usize,
               dynamic_context_mixing: u8,
               literal_adaptation_rate: Option<Speed>) -> Self {
        DivansCodec::<ArithmeticCoder,  Specialization, Cdf16, AllocU8, AllocCDF2, AllocCDF16> {
            cross_command_state:CrossCommandState::<ArithmeticCoder,
                                                    Specialization,
                                                    Cdf16,
                                                    AllocU8,
                                                    AllocCDF2,
                                                    AllocCDF16>::new(m8,
                                                                     mcdf2,
                                                                     mcdf16,
                                                                     coder,
                                                                     specialization,
                                                                     ring_buffer_size,
                                                                     dynamic_context_mixing,
                                                                     literal_adaptation_rate.unwrap_or_else(
                                                                         self::interface::default_literal_speed),
            ),
            state:EncodeOrDecodeState::Begin,
        }
    }
    pub fn get_coder(&self) -> &ArithmeticCoder {
        &self.cross_command_state.coder
    }
    pub fn get_m8(&mut self) -> &mut AllocU8 {
        &mut self.cross_command_state.m8
    }
    pub fn specialization(&mut self) -> &mut Specialization{
        &mut self.cross_command_state.specialization
    }
    pub fn coder(&mut self) -> &mut ArithmeticCoder {
        &mut self.cross_command_state.coder
    }
    pub fn flush(&mut self,
                 output_bytes: &mut [u8],
                 output_bytes_offset: &mut usize) -> BrotliResult{
        loop {
            match self.state {
                EncodeOrDecodeState::Begin => {
                    let mut unused = 0usize;
                    let nop = Command::<AllocU8::AllocatedMemory>::nop();
                    match self.encode_or_decode_one_command(&[],
                                                            &mut unused,
                                                            output_bytes,
                                                            output_bytes_offset,
                                                            &nop,
                                                            true) {
                        OneCommandReturn::BufferExhausted(res) => {
                            match res {
                                BrotliResult::ResultSuccess => {},
                                need => return need,
                            }
                        },
                        OneCommandReturn::Advance => panic!("Unintended state: flush => Advance"),
                    }
                    self.state = EncodeOrDecodeState::EncodedShutdownNode;
                },
                EncodeOrDecodeState::EncodedShutdownNode => {
                    let mut unused = 0usize;
                    match self.cross_command_state.coder.drain_or_fill_internal_buffer(&[], &mut unused, output_bytes, output_bytes_offset) {
                        BrotliResult::ResultSuccess => self.state = EncodeOrDecodeState::ShutdownCoder,
                        ret => return ret,
                    }
                },
                EncodeOrDecodeState::ShutdownCoder => {
                    match self.cross_command_state.coder.close() {
                        BrotliResult::ResultSuccess => self.state = EncodeOrDecodeState::CoderBufferDrain,
                        ret => return ret,
                    }
                },
                EncodeOrDecodeState::CoderBufferDrain => {
                    let mut unused = 0usize;
                    match self.cross_command_state.coder.drain_or_fill_internal_buffer(&[],
                                                                                       &mut unused,
                                                                                       output_bytes,
                                                                                       output_bytes_offset) {
                        BrotliResult::ResultSuccess => {
                            self.state = EncodeOrDecodeState::WriteChecksum(0);
                        },
                        ret => return ret,
                    }
                },
                EncodeOrDecodeState::WriteChecksum(count) => {
                    let bytes_remaining = output_bytes.len() - *output_bytes_offset;
                    let bytes_needed = CHECKSUM_LENGTH - count;
                    let count_to_copy = core::cmp::min(bytes_remaining,
                                                       bytes_needed);
                    let checksum = [b'~',
                                    b'd',
                                    b'i',
                                    b'v',
                                    b'a',
                                    b'n',
                                    b's',
                                    b'~'];
                    output_bytes.split_at_mut(*output_bytes_offset).1.split_at_mut(
                        count_to_copy).0.clone_from_slice(checksum.split_at(count_to_copy).0);
                    *output_bytes_offset += count_to_copy;
                    if bytes_needed <= bytes_remaining {
                        self.state = EncodeOrDecodeState::DivansSuccess;
                        return BrotliResult::ResultSuccess;
                    } else {
                        self.state = EncodeOrDecodeState::WriteChecksum(count + count_to_copy);
                        return BrotliResult::NeedsMoreOutput;
                    }
                },
                EncodeOrDecodeState::DivansSuccess => return BrotliResult::ResultSuccess,
                _ => return self::interface::Fail(), // not allowed to flush if previous command was partially processed
            }
        }
    }
    pub fn encode_or_decode<ISl:SliceWrapper<u8>+Default>(&mut self,
                                                  input_bytes: &[u8],
                                                  input_bytes_offset: &mut usize,
                                                  output_bytes: &mut [u8],
                                                  output_bytes_offset: &mut usize,
                                                  input_commands: &[Command<ISl>],
                                                  input_command_offset: &mut usize) -> BrotliResult {
        loop {
            let i_cmd_backing = Command::<ISl>::nop();
            let in_cmd = self.cross_command_state.specialization.get_input_command(input_commands,
                                                                                   *input_command_offset,
                                                                                   &i_cmd_backing);
            match self.encode_or_decode_one_command(input_bytes,
                                                    input_bytes_offset,
                                                    output_bytes,
                                                    output_bytes_offset,
                                                    in_cmd,
                                                    false /* not end*/) {
                OneCommandReturn::Advance => {
                    *input_command_offset += 1;
                    if input_commands.len() == *input_command_offset {
                        return BrotliResult::NeedsMoreInput;
                    }
                },
                OneCommandReturn::BufferExhausted(result) => {
                    return result;
                }
            }
        }
    }
    pub fn encode_or_decode_one_command<ISl:SliceWrapper<u8>+Default>(&mut self,
                                                  input_bytes: &[u8],
                                                  input_bytes_offset: &mut usize,
                                                  output_bytes: &mut [u8],
                                                  output_bytes_offset: &mut usize,
                                                  input_cmd: &Command<ISl>,
                                                  is_end: bool) -> OneCommandReturn {
        loop {
            let new_state: Option<EncodeOrDecodeState<AllocU8>>;
            match self.state {
                EncodeOrDecodeState::EncodedShutdownNode
                    | EncodeOrDecodeState::ShutdownCoder
                    | EncodeOrDecodeState::CoderBufferDrain
                    | EncodeOrDecodeState::WriteChecksum(_) => {
                    // not allowed to encode additional commands after flush is invoked
                    return OneCommandReturn::BufferExhausted(self::interface::Fail());
                }
                EncodeOrDecodeState::DivansSuccess => {
                    return OneCommandReturn::BufferExhausted(BrotliResult::ResultSuccess);
                },
                EncodeOrDecodeState::Begin => {
                    match self.cross_command_state.coder.drain_or_fill_internal_buffer(input_bytes, input_bytes_offset,
                                                                                      output_bytes, output_bytes_offset) {
                        BrotliResult::ResultSuccess => {},
                        need_something => return OneCommandReturn::BufferExhausted(need_something),
                    }
                    let mut command_type_code = command_type_to_nibble(input_cmd, is_end);
                    {
                        let command_type_prob = self.cross_command_state.bk.get_command_type_prob();
                        self.cross_command_state.coder.get_or_put_nibble(
                            &mut command_type_code,
                            command_type_prob,
                            BillingDesignation::CrossCommand(CrossCommandBilling::FullSelection));
                        command_type_prob.blend(command_type_code, Speed::ROCKET);
                    }
                    let command_state = get_command_state_from_nibble(command_type_code);
                    match command_state {
                        EncodeOrDecodeState::Copy(_) => { self.cross_command_state.bk.obs_copy_state(); },
                        EncodeOrDecodeState::Dict(_) => { self.cross_command_state.bk.obs_dict_state(); },
                        EncodeOrDecodeState::Literal(_) => { self.cross_command_state.bk.obs_literal_state(); },
                        _ => {},
                    }
                    new_state = Some(command_state);
                },
                EncodeOrDecodeState::PredictionMode(ref mut prediction_mode_state) => {
                    let default_prediction_mode_context_map = PredictionModeContextMap::<ISl> {
                        literal_prediction_mode: LiteralPredictionModeNibble::default(),
                        literal_context_map:ISl::default(),
                        distance_context_map:ISl::default(),
                    };
                    let src_pred_mode = match *input_cmd {
                        Command::PredictionMode(ref pm) => pm,
                        _ => &default_prediction_mode_context_map,
                     };
                     match prediction_mode_state.encode_or_decode(&mut self.cross_command_state,
                                                                  src_pred_mode,
                                                                  input_bytes,
                                                                  input_bytes_offset,
                                                                  output_bytes,
                                                                  output_bytes_offset) {
                        BrotliResult::ResultSuccess => new_state = Some(EncodeOrDecodeState::Begin),
                        retval => return OneCommandReturn::BufferExhausted(retval),
                    }
                },
                EncodeOrDecodeState::BlockSwitchLiteral(ref mut block_type_state) => {
                    let src_block_switch_literal = match *input_cmd {
                        Command::BlockSwitchLiteral(bs) => bs,
                        _ => LiteralBlockSwitch::default(),
                    };
                    match block_type_state.encode_or_decode(&mut self.cross_command_state,
                                                            src_block_switch_literal,
                                                            input_bytes,
                                                            input_bytes_offset,
                                                            output_bytes,
                                                            output_bytes_offset) {
                        BrotliResult::ResultSuccess => {
                            self.cross_command_state.bk.obs_btypel(match *block_type_state {
                                block_type::LiteralBlockTypeState::FullyDecoded(btype, stride) => LiteralBlockSwitch::new(btype, stride),
                                _ => panic!("illegal output state"),
                            });
                            new_state = Some(EncodeOrDecodeState::Begin);
                        },
                        retval => {
                            return OneCommandReturn::BufferExhausted(retval);
                        }
                    }
                },
                EncodeOrDecodeState::BlockSwitchCommand(ref mut block_type_state) => {
                    let src_block_switch_command = match *input_cmd {
                        Command::BlockSwitchCommand(bs) => bs,
                        _ => BlockSwitch::default(),
                    };
                    match block_type_state.encode_or_decode(&mut self.cross_command_state,
                                                            src_block_switch_command,
                                                            self::interface::BLOCK_TYPE_COMMAND_SWITCH,
                                                            input_bytes,
                                                            input_bytes_offset,
                                                            output_bytes,
                                                            output_bytes_offset) {
                        BrotliResult::ResultSuccess => {
                            self.cross_command_state.bk.obs_btypec(match *block_type_state {
                                block_type::BlockTypeState::FullyDecoded(btype) => btype,
                                _ => panic!("illegal output state"),
                            });
                            new_state = Some(EncodeOrDecodeState::Begin);
                        },
                        retval => {
                            return OneCommandReturn::BufferExhausted(retval);
                        }
                    }
                },
                EncodeOrDecodeState::BlockSwitchDistance(ref mut block_type_state) => {
                    let src_block_switch_distance = match *input_cmd {
                        Command::BlockSwitchDistance(bs) => bs,
                        _ => BlockSwitch::default(),
                    };

                    match block_type_state.encode_or_decode(&mut self.cross_command_state,
                                                            src_block_switch_distance,
                                                            self::interface::BLOCK_TYPE_DISTANCE_SWITCH,
                                                            input_bytes,
                                                            input_bytes_offset,
                                                            output_bytes,
                                                            output_bytes_offset) {
                        BrotliResult::ResultSuccess => {
                            self.cross_command_state.bk.obs_btyped(match *block_type_state {
                                block_type::BlockTypeState::FullyDecoded(btype) => btype,
                                _ => panic!("illegal output state"),
                            });
                            new_state = Some(EncodeOrDecodeState::Begin);
                        },
                        retval => {
                            return OneCommandReturn::BufferExhausted(retval);
                        }
                    }
                },
                EncodeOrDecodeState::Copy(ref mut copy_state) => {
                    let backing_store = CopyCommand::nop();
                    let src_copy_command = self.cross_command_state.specialization.get_source_copy_command(input_cmd,
                                                                                                           &backing_store);
                    match copy_state.encode_or_decode(&mut self.cross_command_state,
                                                      src_copy_command,
                                                      input_bytes,
                                                      input_bytes_offset,
                                                      output_bytes,
                                                      output_bytes_offset
                                                      ) {
                        BrotliResult::ResultSuccess => {
                            self.cross_command_state.bk.obs_distance(&copy_state.cc);
                            new_state = Some(EncodeOrDecodeState::PopulateRingBuffer(
                                Command::Copy(core::mem::replace(&mut copy_state.cc,
                                                                 CopyCommand::nop()))));
                        },
                        retval => {
                            return OneCommandReturn::BufferExhausted(retval);
                        }
                    }
                },
                EncodeOrDecodeState::Literal(ref mut lit_state) => {
                    let backing_store = LiteralCommand::nop();
                    let src_literal_command = self.cross_command_state.specialization.get_source_literal_command(input_cmd,
                                                                                                                 &backing_store);
                    match lit_state.encode_or_decode(&mut self.cross_command_state,
                                                      src_literal_command,
                                                      input_bytes,
                                                      input_bytes_offset,
                                                      output_bytes,
                                                      output_bytes_offset
                                                      ) {
                        BrotliResult::ResultSuccess => {
                            new_state = Some(EncodeOrDecodeState::PopulateRingBuffer(
                                Command::Literal(core::mem::replace(&mut lit_state.lc,
                                                                    LiteralCommand::<AllocatedMemoryPrefix<AllocU8>>::nop()))));
                        },
                        retval => {
                            return OneCommandReturn::BufferExhausted(retval);
                        }
                    }
                },
                EncodeOrDecodeState::Dict(ref mut dict_state) => {
                    let backing_store = DictCommand::nop();
                    let src_dict_command = self.cross_command_state.specialization.get_source_dict_command(input_cmd,
                                                                                                                 &backing_store);
                    match dict_state.encode_or_decode(&mut self.cross_command_state,
                                                      src_dict_command,
                                                      input_bytes,
                                                      input_bytes_offset,
                                                      output_bytes,
                                                      output_bytes_offset
                                                      ) {
                        BrotliResult::ResultSuccess => {
                            new_state = Some(EncodeOrDecodeState::PopulateRingBuffer(
                                Command::Dict(core::mem::replace(&mut dict_state.dc,
                                                                 DictCommand::nop()))));
                        },
                        retval => {
                            return OneCommandReturn::BufferExhausted(retval);
                        }
                    }
                },
                EncodeOrDecodeState::PopulateRingBuffer(ref mut o_cmd) => {
                    let mut tmp_output_offset_bytes_backing: usize = 0;
                    let mut tmp_output_offset_bytes = self.cross_command_state.specialization.get_recoder_output_offset(
                        output_bytes_offset,
                        &mut tmp_output_offset_bytes_backing);
                    match self.cross_command_state.recoder.encode_cmd(o_cmd,
                                                                  self.cross_command_state.
                                                                  specialization.get_recoder_output(output_bytes),
                                                                  tmp_output_offset_bytes) {
                        BrotliResult::NeedsMoreInput => panic!("Unexpected return value"),//new_state = Some(EncodeOrDecodeState::Begin),
                        BrotliResult::NeedsMoreOutput => {
                            self.cross_command_state.bk.decode_byte_count = self.cross_command_state.recoder.num_bytes_encoded() as u32;
                            if self.cross_command_state.specialization.does_caller_want_original_file_bytes() {
                                return OneCommandReturn::BufferExhausted(BrotliResult::NeedsMoreOutput); // we need the caller to drain the buffer
                            }
                            new_state = None;
                        },
                        BrotliResult::ResultFailure => {
                            self.cross_command_state.bk.decode_byte_count = self.cross_command_state.recoder.num_bytes_encoded() as u32;
                            return OneCommandReturn::BufferExhausted(self::interface::Fail());
                        },
                        BrotliResult::ResultSuccess => {
                            self.cross_command_state.bk.command_count += 1;
                            self.cross_command_state.bk.decode_byte_count = self.cross_command_state.recoder.num_bytes_encoded() as u32;
                            // clobber bk.last_8_literals with the last 8 literals
                            let last_8 = self.cross_command_state.recoder.last_8_literals();
                            self.cross_command_state.bk.last_8_literals =
                                u64::from(last_8[0])
                                | (u64::from(last_8[1])<<0x8)
                                | (u64::from(last_8[2])<<0x10)
                                | (u64::from(last_8[3])<<0x18)
                                | (u64::from(last_8[4])<<0x20)
                                | (u64::from(last_8[5])<<0x28)
                                | (u64::from(last_8[6])<<0x30)
                                | (u64::from(last_8[7])<<0x38);
                            new_state = Some(EncodeOrDecodeState::Begin);
                            if let Command::Literal(ref mut l) = *o_cmd {
                                let mfd = core::mem::replace(&mut l.data,
                                                             AllocatedMemoryPrefix::<AllocU8>::default()).0;
                                self.cross_command_state.m8.free_cell(mfd);
                                //FIXME: what about prob array: should that be freed
                            }
                        },
                    }
                },
            }
            if let Some(ns) = new_state {
                match ns {
                    EncodeOrDecodeState::Begin => {
                        self.state = EncodeOrDecodeState::Begin;
                        return OneCommandReturn::Advance;
                    },
                    _ => self.state = ns,
                }
            }
        }
    }
}