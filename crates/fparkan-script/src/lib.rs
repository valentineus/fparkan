#![forbid(unsafe_code)]
//! Lossless, bounded reader for compiled AI `.scr` packages.
//!
//! This module preserves the layout proven by the GOG `ai.dll` reader.  It
//! deliberately does not implement a general bytecode VM. It contains a
//! narrow resolver for confirmed `Init` handlers and bounded FML expressions.

use fparkan_binary::{checked_allocation_len, Cursor, DecodeError, Limits};
use std::sync::Arc;

const INSTRUCTION_HEADER_BYTES: u64 = 28;
const INSTRUCTION_WORDS: usize = 7;
const GOG_HANDLER_COUNT: u32 = 73;
const MAX_VARSET_DECLARATIONS: usize = 4096;

/// Parsed defaults from the text `varset.var` source shared by AI packages.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct VarSet {
    /// Declarations in original source order.
    pub declarations: Vec<VarSetDeclaration>,
}

/// One supported `VAR(...)` declaration from `varset.var`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VarSetDeclaration {
    /// Variable type spelling used by the original source.
    pub type_name: VarSetType,
    /// ASCII variable name.
    pub name: String,
    /// Typed default value.
    pub default_value: VarSetDefault,
}

/// Numeric declaration types present in the shipped GOG `varset.var`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VarSetType {
    /// `VAR(float, ...)`.
    Float,
    /// `VAR(DWORD, ...)`.
    Dword,
}

/// A numeric cell value that preserves the declared `varset.var` type.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VarSetDefault {
    /// IEEE-754 bits parsed from a `float` literal.
    FloatBits(u32),
    /// Unsigned 32-bit integer parsed from a decimal or hexadecimal `DWORD` literal.
    Dword(u32),
}

/// One opaque command delivered by a VM handler to the host-supplied callback.
///
/// The numeric mode belongs to the callback ABI; it has no inferred gameplay
/// meaning yet.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VmHostCallbackCommand {
    /// First callback ABI word.
    pub mode: u32,
    /// First resolved callback payload word.
    pub first: u32,
    /// Second resolved callback payload word.
    pub second: u32,
}

/// The optional target-lookup payload selected by the recovered `Handler(15)`
/// mode word. Names follow the shipped `varset.var` constants; they do not
/// assign a gameplay operation to the later opaque virtual call.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Handler15TargetPayload {
    /// `NONE` (`0`): no additional reference is consumed.
    None,
    /// `TARGET_BY_LOGIC_ID` (`0x0201`): one DWORD payload.
    ByLogicId(u32),
    /// `TARGET_BY_PLACE` (`0x0202`): two DWORD payloads.
    ByPlace {
        /// First DWORD payload in source order.
        first: u32,
        /// Second DWORD payload in source order.
        second: u32,
    },
    /// `TARGET_BY_TYPE` (`0x0203`): one DWORD payload.
    ByType(u32),
    /// `TARGET_NOT_DEFINED` (`0x0204`): one DWORD payload.
    NotDefined(u32),
    /// `TARGET_BY_NAME` (`0x0205`): one DWORD payload.
    ByName(u32),
}

/// Resolved fixed-width operands delivered by `Handler(15)` to its target
/// interface.
///
/// Slot names preserve disk/reference order. The original first looks up an
/// opaque target through `word_0`, then calls a target vtable slot with this
/// packed data and `word_2`; neither operation is named here.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Handler15Invocation {
    /// Resolved DWORD reference 0, used for the preceding target lookup.
    pub word_0: u32,
    /// Resolved DWORD reference 1.
    pub word_1: u32,
    /// Resolved DWORD reference 2, passed separately to the target call.
    pub word_2: u32,
    /// Resolved DWORD reference 3.
    pub word_3: u32,
    /// Resolved scalar reference 4.
    pub scalar_4: f32,
    /// Resolved scalar reference 5.
    pub scalar_5: f32,
    /// Resolved scalar reference 6.
    pub scalar_6: f32,
    /// Resolved scalar reference 7.
    pub scalar_7: f32,
    /// Target mode and its exactly consumed trailing DWORD references.
    pub target: Handler15TargetPayload,
}

/// The proven reset branch selected before a `Handler(8)` state write.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Handler8Reset {
    /// No reset helper is called before the state field is written.
    None,
    /// State `2`: helper resets record words `0..=3` and `6`, then invokes
    /// two still-opaque callback slots.
    StateTwo,
    /// State `3`: helper resets record words `0..=2` and `6`, then invokes
    /// the same callback slots.
    StateThree,
}

/// One opaque 100-byte AI-record state transition requested by `Handler(8)`.
///
/// The record index is the runtime value of original `dCurrentProblem`; it is
/// intentionally separate from a compiled instruction reference.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Handler8StateChange {
    /// Checked `dCurrentProblem` record index in the handler's `this + 0xa0` table.
    pub record_index: u32,
    /// Resolved instruction DWORD written at record offset `+0x18`.
    pub next_state: u32,
    /// Proven pre-write reset branch.
    pub reset: Handler8Reset,
}

/// One raw DWORD write made by the corpus-proven `Handler(19)` Init path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Handler19DwordWrite {
    /// Compiled varset record index selected by the instruction reference.
    pub index: u32,
    /// Exact 32-bit payload passed to the original common varset setter.
    pub value: u32,
}

/// A typed variable write emitted by the recovered first-`Init` subset.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ScriptInitWrite {
    /// Compiled varset declaration index.
    pub index: u32,
    /// Value in the original cell's numeric representation.
    pub value: VarSetDefault,
}

/// Result of resolving the first case-sensitive `Init` event.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ScriptInitResolution {
    /// No event has the case-sensitive C-string name `Init`.
    Missing,
    /// The first `Init` event completed within the recovered subset.
    Executed {
        /// Zero-based selected event index.
        event_index: usize,
        /// Opaque event word retained from the package.
        event_word: u32,
        /// Ordered, typed writes emitted by the event.
        writes: Vec<ScriptInitWrite>,
    },
    /// The event reached a known but unsupported form; earlier writes are discarded.
    Unsupported {
        /// Zero-based selected event index.
        event_index: usize,
        /// Zero-based unsupported instruction index.
        instruction_index: usize,
        /// Unsupported instruction or expression form.
        error: ScriptInitResolveError,
    },
}

/// A typed failure or unsupported form in the recovered `Init` subset.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ScriptInitResolveError {
    /// The selector is outside the recovered `Handler(19)` and formula assignment paths.
    UnsupportedSelector {
        /// Observed selector.
        found: ScriptDispatchSelector,
    },
    /// A `Handler(19)` reference or declaration did not match its recovered contract.
    Handler19(Handler19ResolveError),
    /// A formula record has an unsupported control word.
    UnsupportedFormulaControlWord {
        /// Raw disk word at index 3.
        found: u32,
    },
    /// A formula record requests the separate whole-varset-record copy path.
    UnsupportedFormulaCopySource {
        /// Raw disk word at index 2.
        found: u32,
    },
    /// The selected script base did not provide an FML table.
    MissingFormulaTable,
    /// The FML ordinal is outside the preserved row table.
    FormulaIndexOutOfBounds {
        /// Zero-based FML row index.
        index: u32,
        /// Number of preserved FML rows.
        rows: usize,
    },
    /// The referenced FML row is outside the supported atom-expression subset.
    UnsupportedFormulaExpression {
        /// Zero-based FML row index.
        index: u32,
        /// Raw expression bytes from the row.
        expression: Vec<u8>,
    },
    /// A DWORD target cannot represent the native truncating qword conversion.
    DwordConversionOutOfRange {
        /// Compiled target index.
        index: u32,
        /// Evaluated binary32 payload.
        value_bits: u32,
    },
    /// The formula record target is outside the selected varset.
    VarSetIndexOutOfBounds {
        /// Compiled target index.
        index: u32,
        /// Available declaration count.
        declarations: usize,
    },
}

impl std::fmt::Display for ScriptInitResolveError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedSelector { found } => {
                write!(formatter, "unsupported script selector {found:?} in Init")
            }
            Self::Handler19(source) => write!(formatter, "Handler(19): {source}"),
            Self::UnsupportedFormulaControlWord { found } => write!(
                formatter,
                "unsupported formula control word 0x{found:08x}; expected 0xffffffff"
            ),
            Self::UnsupportedFormulaCopySource { found } => write!(
                formatter,
                "unsupported formula source 0x{found:08x}; whole-varset copy is not modeled"
            ),
            Self::MissingFormulaTable => write!(formatter, "formula assignment has no FML table"),
            Self::FormulaIndexOutOfBounds { index, rows } => write!(
                formatter,
                "formula index {index} is outside the FML table with {rows} rows"
            ),
            Self::UnsupportedFormulaExpression { index, expression } => write!(
                formatter,
                "formula {index} expression {:?} is outside the supported atom and binary +/− subset",
                String::from_utf8_lossy(expression)
            ),
            Self::DwordConversionOutOfRange { index, value_bits } => write!(
                formatter,
                "formula value 0x{value_bits:08x} cannot be converted to DWORD target {index}"
            ),
            Self::VarSetIndexOutOfBounds {
                index,
                declarations,
            } => write!(
                formatter,
                "formula target {index} is outside {declarations} varset declarations"
            ),
        }
    }
}

impl std::error::Error for ScriptInitResolveError {}

enum InitInstructionResolution {
    Executed(Vec<ScriptInitWrite>),
    Unsupported(ScriptInitResolveError),
}

/// Rows loaded from one script base's `.fml` file in native table order.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FormulaTable {
    /// Accepted `FUNCTION` rows in their original order, including unsupported expressions.
    pub rows: Vec<FormulaRow>,
}

/// One source row accepted by the native FML line loader.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FormulaRow {
    /// Name token retained from the first comma-delimited field.
    pub name_raw: Vec<u8>,
    /// Expression bytes from the second field; interpretation is lazy.
    pub expression_raw: Vec<u8>,
    /// Final field bytes, retained without interpreting their meaning.
    pub trailing_raw: Vec<u8>,
}

/// Inputs already expressed at the original `Handler(19)` setter ABI boundary.
///
/// The first two words are results of original x87 `__ftol` calls. Keeping
/// them as words prevents this package from silently substituting Rust's float
/// conversion policy before captured x87 vectors are available.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Handler19InitInput {
    /// x87-converted value from VM field `+0x80`.
    pub first_x87_word: u32,
    /// x87-converted value from VM field `+0x84`.
    pub second_x87_word: u32,
    /// Raw word from VM field `+0x7c`.
    pub third_word: u32,
}

/// Error resolving the three-target `Handler(19)` Init contract.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Handler19ResolveError {
    /// The selected action is not the recovered `Handler(19)` slot.
    UnsupportedSelector {
        /// Recovered selector shape.
        found: ScriptDispatchSelector,
    },
    /// Only action instructions with control word `0xffff_ffff` are supported.
    UnsupportedControlWord {
        /// Opaque control word from disk-order header index 3.
        found: u32,
    },
    /// Only instructions with no result-save reference are supported.
    UnsupportedResultSave {
        /// Opaque variable reference from disk-order header index 1.
        found: u32,
    },
    /// One of the three required references was absent.
    MissingReference {
        /// Zero-based required target position.
        position: usize,
    },
    /// A compiled reference indexed outside the loaded varset.
    VarSetIndexOutOfBounds {
        /// Referenced compiled-varset index.
        index: u32,
        /// Available declaration count.
        declarations: usize,
    },
    /// The `AutoDemo` Init target was not a `DWORD` declaration.
    UnexpectedType {
        /// Zero-based target position.
        position: usize,
        /// Observed declaration type.
        found: VarSetType,
    },
}

impl std::fmt::Display for Handler19ResolveError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedSelector { found } => {
                write!(formatter, "unsupported script selector {found:?} in Init")
            }
            Self::UnsupportedControlWord { found } => write!(
                formatter,
                "unsupported Init control word 0x{found:08x}; expected 0xffffffff"
            ),
            Self::UnsupportedResultSave { found } => write!(
                formatter,
                "unsupported Init result-save reference 0x{found:08x}; expected 0xffffffff"
            ),
            Self::MissingReference { position } => {
                write!(formatter, "Handler(19) is missing reference {position}")
            }
            Self::VarSetIndexOutOfBounds {
                index,
                declarations,
            } => write!(
                formatter,
                "Handler(19) reference {index} is outside {declarations} varset declarations"
            ),
            Self::UnexpectedType { position, found } => write!(
                formatter,
                "Handler(19) reference {position} has {found:?}, expected Dword"
            ),
        }
    }
}

impl std::error::Error for Handler19ResolveError {}

/// Error resolving the one-operand `Handler(8)` contract.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Handler8ResolveError {
    /// The instruction omitted its required DWORD reference.
    MissingReference,
    /// The instruction reference is outside the loaded varset.
    VarSetIndexOutOfBounds {
        /// Referenced compiled-varset index.
        index: u32,
        /// Available declaration count.
        declarations: usize,
    },
    /// The reference was not a DWORD declaration.
    UnexpectedType {
        /// Observed declaration type.
        found: VarSetType,
    },
}

impl std::fmt::Display for Handler8ResolveError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingReference => write!(formatter, "Handler(8) is missing reference 0"),
            Self::VarSetIndexOutOfBounds {
                index,
                declarations,
            } => write!(
                formatter,
                "Handler(8) reference {index} is outside {declarations} varset declarations"
            ),
            Self::UnexpectedType { found } => {
                write!(
                    formatter,
                    "Handler(8) reference 0 has {found:?}, expected Dword"
                )
            }
        }
    }
}

impl std::error::Error for Handler8ResolveError {}

/// Error resolving the corpus-proven `Handler(15)` input layout.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Handler15ResolveError {
    /// One of the mode-specific required references was absent.
    MissingReference {
        /// Zero-based required reference position.
        position: usize,
    },
    /// A compiled reference indexed outside the loaded varset.
    VarSetIndexOutOfBounds {
        /// Referenced compiled-varset index.
        index: u32,
        /// Available declaration count.
        declarations: usize,
    },
    /// A reference's declaration type differed from the observed contract.
    UnexpectedType {
        /// Zero-based reference position.
        position: usize,
        /// Type required by the recovered handler contract.
        expected: VarSetType,
        /// Type actually declared in the loaded varset.
        found: VarSetType,
    },
    /// The target-mode value was not among the modes observed in the GOG corpus.
    UnsupportedTargetMode(u32),
}

impl std::fmt::Display for Handler15ResolveError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingReference { position } => {
                write!(formatter, "Handler(15) is missing reference {position}")
            }
            Self::VarSetIndexOutOfBounds {
                index,
                declarations,
            } => write!(
                formatter,
                "Handler(15) reference {index} is outside {declarations} varset declarations"
            ),
            Self::UnexpectedType {
                position,
                expected,
                found,
            } => write!(
                formatter,
                "Handler(15) reference {position} has {found:?}, expected {expected:?}"
            ),
            Self::UnsupportedTargetMode(mode) => {
                write!(
                    formatter,
                    "Handler(15) has unsupported target mode 0x{mode:08x}"
                )
            }
        }
    }
}

impl std::error::Error for Handler15ResolveError {}

/// Error resolving the proven `Handler(30)` callback contract.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Handler30ResolveError {
    /// The instruction did not contain one of Handler(30)'s required references.
    MissingReference {
        /// Zero-based required reference position.
        position: usize,
    },
    /// A reference index was outside the loaded varset.
    VarSetIndexOutOfBounds {
        /// Referenced index from the compiled instruction.
        index: u32,
        /// Available declaration count.
        declarations: usize,
    },
    /// A `float` declaration would require the still-unrecovered x87 `__ftol` policy.
    FloatRequiresX87 {
        /// Referenced index from the compiled instruction.
        index: u32,
    },
}

impl std::fmt::Display for Handler30ResolveError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingReference { position } => {
                write!(formatter, "Handler(30) is missing reference {position}")
            }
            Self::VarSetIndexOutOfBounds {
                index,
                declarations,
            } => write!(
                formatter,
                "Handler(30) reference {index} is outside {declarations} varset declarations"
            ),
            Self::FloatRequiresX87 { index } => write!(
                formatter,
                "Handler(30) reference {index} requires unrecovered x87 float-to-u32 conversion"
            ),
        }
    }
}

impl std::error::Error for Handler30ResolveError {}

impl VarSet {
    /// Resolves the two index operands consumed by the GOG `Handler(30)`.
    ///
    /// The original invokes its host callback with mode zero after resolving
    /// both operands through `FUN_10013570`. The shipped GOG corpus uses only
    /// `DWORD` declarations at these positions. A float is rejected until a
    /// captured x87 `__ftol` conversion profile exists.
    ///
    /// # Errors
    ///
    /// Returns a typed error for missing references, out-of-range varset
    /// indices, or a float operand requiring unrecovered x87 behavior.
    pub fn resolve_handler30(
        &self,
        instruction: &ScriptInstruction,
    ) -> Result<VmHostCallbackCommand, Handler30ResolveError> {
        let values: Vec<_> = self
            .declarations
            .iter()
            .map(|declaration| declaration.default_value)
            .collect();
        self.resolve_handler30_with_values(instruction, &values)
    }

    /// Resolves `Handler(30)` against instantiated runtime varset values.
    ///
    /// The declaration table still establishes the original numeric kind; the
    /// supplied cells carry state after prior recovered handlers have written
    /// it. This is the execution-time counterpart of [`Self::resolve_handler30`].
    ///
    /// # Errors
    ///
    /// Returns the same bounded errors as [`Self::resolve_handler30`] when an
    /// instruction, declaration, or runtime cell cannot satisfy the proven
    /// DWORD operand contract.
    pub fn resolve_handler30_with_values(
        &self,
        instruction: &ScriptInstruction,
        values: &[VarSetDefault],
    ) -> Result<VmHostCallbackCommand, Handler30ResolveError> {
        let first = self.resolve_handler30_operand_with_values(instruction, values, 0)?;
        let second = self.resolve_handler30_operand_with_values(instruction, values, 1)?;
        Ok(VmHostCallbackCommand {
            mode: 0,
            first,
            second,
        })
    }

    fn resolve_handler30_operand_with_values(
        &self,
        instruction: &ScriptInstruction,
        values: &[VarSetDefault],
        position: usize,
    ) -> Result<u32, Handler30ResolveError> {
        let index = *instruction
            .references
            .get(position)
            .ok_or(Handler30ResolveError::MissingReference { position })?;
        let declaration = self.declarations.get(index as usize).ok_or(
            Handler30ResolveError::VarSetIndexOutOfBounds {
                index,
                declarations: self.declarations.len(),
            },
        )?;
        let value =
            values
                .get(index as usize)
                .ok_or(Handler30ResolveError::VarSetIndexOutOfBounds {
                    index,
                    declarations: values.len(),
                })?;
        match (declaration.type_name, value) {
            (VarSetType::Dword, VarSetDefault::Dword(value)) => Ok(*value),
            (VarSetType::Float, _) | (_, VarSetDefault::FloatBits(_)) => {
                Err(Handler30ResolveError::FloatRequiresX87 { index })
            }
        }
    }

    /// Resolves the one DWORD operand consumed by GOG `Handler(8)`.
    ///
    /// `current_problem_record_index` is the caller's live value of the
    /// loader-bound `dCurrentProblem` varset entry. The original bounds-checks
    /// it against a table at `this + 0xa0`; table ownership and callback
    /// semantics remain outside this package-level resolver.
    ///
    /// # Errors
    ///
    /// Returns a typed error instead of coercing a missing, invalid, or float
    /// reference into a state transition.
    pub fn resolve_handler8(
        &self,
        instruction: &ScriptInstruction,
        current_problem_record_index: u32,
    ) -> Result<Handler8StateChange, Handler8ResolveError> {
        let index = *instruction
            .references
            .first()
            .ok_or(Handler8ResolveError::MissingReference)?;
        let declaration = self.declarations.get(index as usize).ok_or(
            Handler8ResolveError::VarSetIndexOutOfBounds {
                index,
                declarations: self.declarations.len(),
            },
        )?;
        let next_state = match declaration {
            VarSetDeclaration {
                type_name: VarSetType::Dword,
                default_value: VarSetDefault::Dword(value),
                ..
            } => *value,
            declaration => {
                return Err(Handler8ResolveError::UnexpectedType {
                    found: declaration.type_name,
                });
            }
        };
        let reset = match next_state {
            2 => Handler8Reset::StateTwo,
            3 => Handler8Reset::StateThree,
            _ => Handler8Reset::None,
        };
        Ok(Handler8StateChange {
            record_index: current_problem_record_index,
            next_state,
            reset,
        })
    }

    /// Resolves the three DWORD targets written by GOG `Handler(19)`.
    ///
    /// The caller supplies values at the original setter ABI boundary: the
    /// first two are already x87-converted `__ftol` results and the third is
    /// the raw VM word. This preserves the exact default-script Init path
    /// without claiming an unrecovered portable x87 policy.
    ///
    /// # Errors
    ///
    /// Returns a typed failure for missing/out-of-range targets or a target
    /// type that differs from the observed `AutoDemo` `DWORD` contract.
    pub fn resolve_handler19(
        &self,
        instruction: &ScriptInstruction,
        input: Handler19InitInput,
    ) -> Result<[Handler19DwordWrite; 3], Handler19ResolveError> {
        let targets = [
            self.resolve_handler19_target(instruction, 0)?,
            self.resolve_handler19_target(instruction, 1)?,
            self.resolve_handler19_target(instruction, 2)?,
        ];
        Ok([
            Handler19DwordWrite {
                index: targets[0],
                value: input.first_x87_word,
            },
            Handler19DwordWrite {
                index: targets[1],
                value: input.second_x87_word,
            },
            Handler19DwordWrite {
                index: targets[2],
                value: input.third_word,
            },
        ])
    }

    fn resolve_handler19_target(
        &self,
        instruction: &ScriptInstruction,
        position: usize,
    ) -> Result<u32, Handler19ResolveError> {
        let index = *instruction
            .references
            .get(position)
            .ok_or(Handler19ResolveError::MissingReference { position })?;
        match self.declarations.get(index as usize) {
            Some(VarSetDeclaration {
                type_name: VarSetType::Dword,
                ..
            }) => Ok(index),
            Some(declaration) => Err(Handler19ResolveError::UnexpectedType {
                position,
                found: declaration.type_name,
            }),
            None => Err(Handler19ResolveError::VarSetIndexOutOfBounds {
                index,
                declarations: self.declarations.len(),
            }),
        }
    }

    /// Resolves the first case-sensitive `Init` event's recovered write subset.
    ///
    /// Handler(19) writes use the caller's original setter-boundary words. A
    /// sentinel selector is accepted only for the proven formula assignment
    /// with control and copy-source words both equal to `0xffffffff`; the
    /// referenced row is evaluated only when that instruction is reached.
    /// Formula atoms read defaults and earlier successful writes in this event.
    ///
    /// # Errors
    ///
    /// Returns a typed error for malformed Handler(19) references, invalid
    /// formula or varset indices, and targets outside the selected varset.
    pub fn resolve_init(
        &self,
        package: &ScriptPackage,
        input: Handler19InitInput,
        formulas: Option<&FormulaTable>,
    ) -> Result<ScriptInitResolution, ScriptInitResolveError> {
        let Some((event_index, event)) = package.events.iter().enumerate().find(|(_, event)| {
            event.name_raw.split(|byte| *byte == 0).next() == Some(&b"Init"[..])
        }) else {
            return Ok(ScriptInitResolution::Missing);
        };

        let mut values = self
            .declarations
            .iter()
            .map(|declaration| declaration.default_value)
            .collect::<Vec<_>>();
        let mut writes = Vec::new();
        for (instruction_index, instruction) in event.instructions.iter().enumerate() {
            match self.resolve_init_instruction(instruction, input, formulas, &values)? {
                InitInstructionResolution::Executed(instruction_writes) => {
                    for write in &instruction_writes {
                        let Some(value) = values.get_mut(write.index as usize) else {
                            return Err(ScriptInitResolveError::VarSetIndexOutOfBounds {
                                index: write.index,
                                declarations: self.declarations.len(),
                            });
                        };
                        *value = write.value;
                    }
                    writes.extend(instruction_writes);
                }
                InitInstructionResolution::Unsupported(error) => {
                    return Ok(ScriptInitResolution::Unsupported {
                        event_index,
                        instruction_index,
                        error,
                    });
                }
            }
        }

        Ok(ScriptInitResolution::Executed {
            event_index,
            event_word: event.event_word,
            writes,
        })
    }

    fn resolve_init_instruction(
        &self,
        instruction: &ScriptInstruction,
        input: Handler19InitInput,
        formulas: Option<&FormulaTable>,
        values: &[VarSetDefault],
    ) -> Result<InitInstructionResolution, ScriptInitResolveError> {
        match instruction.dispatch_selector() {
            ScriptDispatchSelector::Handler(19) => {
                if instruction.header_words[3] != u32::MAX {
                    return Ok(InitInstructionResolution::Unsupported(
                        ScriptInitResolveError::Handler19(
                            Handler19ResolveError::UnsupportedControlWord {
                                found: instruction.header_words[3],
                            },
                        ),
                    ));
                }
                if instruction.header_words[1] != u32::MAX {
                    return Ok(InitInstructionResolution::Unsupported(
                        ScriptInitResolveError::Handler19(
                            Handler19ResolveError::UnsupportedResultSave {
                                found: instruction.header_words[1],
                            },
                        ),
                    ));
                }
                match self.resolve_handler19(instruction, input) {
                    Ok(writes) => Ok(InitInstructionResolution::Executed(
                        writes
                            .into_iter()
                            .map(|write| ScriptInitWrite {
                                index: write.index,
                                value: VarSetDefault::Dword(write.value),
                            })
                            .collect(),
                    )),
                    Err(error @ Handler19ResolveError::UnsupportedControlWord { .. })
                    | Err(error @ Handler19ResolveError::UnsupportedResultSave { .. }) => {
                        Ok(InitInstructionResolution::Unsupported(
                            ScriptInitResolveError::Handler19(error),
                        ))
                    }
                    Err(error) => Err(ScriptInitResolveError::Handler19(error)),
                }
            }
            ScriptDispatchSelector::Sentinel => {
                if instruction.header_words[3] != u32::MAX {
                    return Ok(InitInstructionResolution::Unsupported(
                        ScriptInitResolveError::UnsupportedFormulaControlWord {
                            found: instruction.header_words[3],
                        },
                    ));
                }
                if instruction.header_words[2] != u32::MAX {
                    return Ok(InitInstructionResolution::Unsupported(
                        ScriptInitResolveError::UnsupportedFormulaCopySource {
                            found: instruction.header_words[2],
                        },
                    ));
                }
                let Some(formulas) = formulas else {
                    return Ok(InitInstructionResolution::Unsupported(
                        ScriptInitResolveError::MissingFormulaTable,
                    ));
                };
                let formula_index = instruction.header_words[6];
                let Some(formula) = formulas.rows.get(formula_index as usize) else {
                    return Err(ScriptInitResolveError::FormulaIndexOutOfBounds {
                        index: formula_index,
                        rows: formulas.rows.len(),
                    });
                };
                let Some(value) = parse_init_formula_expression(
                    &formula.expression_raw,
                    &self.declarations,
                    values,
                ) else {
                    return Ok(InitInstructionResolution::Unsupported(
                        ScriptInitResolveError::UnsupportedFormulaExpression {
                            index: formula_index,
                            expression: formula.expression_raw.clone(),
                        },
                    ));
                };
                let target_index = instruction.header_words[1];
                let Some(target) = self.declarations.get(target_index as usize) else {
                    return Err(ScriptInitResolveError::VarSetIndexOutOfBounds {
                        index: target_index,
                        declarations: self.declarations.len(),
                    });
                };
                let Some(current_target) = values.get(target_index as usize) else {
                    return Err(ScriptInitResolveError::VarSetIndexOutOfBounds {
                        index: target_index,
                        declarations: values.len(),
                    });
                };
                if !varset_value_matches_type(target.type_name, *current_target) {
                    return Ok(InitInstructionResolution::Unsupported(
                        ScriptInitResolveError::UnsupportedFormulaExpression {
                            index: formula_index,
                            expression: formula.expression_raw.clone(),
                        },
                    ));
                }
                let value = match target.type_name {
                    VarSetType::Float => VarSetDefault::FloatBits(value.to_bits()),
                    VarSetType::Dword => {
                        let Some(value) = formula_dword_value(value) else {
                            return Ok(InitInstructionResolution::Unsupported(
                                ScriptInitResolveError::DwordConversionOutOfRange {
                                    index: target_index,
                                    value_bits: value.to_bits(),
                                },
                            ));
                        };
                        VarSetDefault::Dword(value)
                    }
                };
                Ok(InitInstructionResolution::Executed(vec![ScriptInitWrite {
                    index: target_index,
                    value,
                }]))
            }
            found => Ok(InitInstructionResolution::Unsupported(
                ScriptInitResolveError::UnsupportedSelector { found },
            )),
        }
    }

    /// Resolves the fixed-width, corpus-proven operands of GOG `Handler(15)`.
    ///
    /// Static analysis of `ai.dll` at `0x10008054` shows four DWORD slots,
    /// four scalar slots, then a DWORD mode. The mode selects zero, one, or
    /// two additional DWORD references. This method deliberately stops before
    /// the original target lookup and virtual call because their object and
    /// gameplay semantics are not yet recovered.
    ///
    /// # Errors
    ///
    /// Returns typed failures for missing/out-of-range operands, type changes,
    /// or an unobserved target mode rather than silently inventing a command.
    pub fn resolve_handler15(
        &self,
        instruction: &ScriptInstruction,
    ) -> Result<Handler15Invocation, Handler15ResolveError> {
        let word_0 = self.resolve_handler15_dword(instruction, 0)?;
        let word_1 = self.resolve_handler15_dword(instruction, 1)?;
        let word_2 = self.resolve_handler15_dword(instruction, 2)?;
        let word_3 = self.resolve_handler15_dword(instruction, 3)?;
        let scalar_4 = self.resolve_handler15_float(instruction, 4)?;
        let scalar_5 = self.resolve_handler15_float(instruction, 5)?;
        let scalar_6 = self.resolve_handler15_float(instruction, 6)?;
        let scalar_7 = self.resolve_handler15_float(instruction, 7)?;
        let target = match self.resolve_handler15_dword(instruction, 8)? {
            0 => Handler15TargetPayload::None,
            0x0201 => {
                Handler15TargetPayload::ByLogicId(self.resolve_handler15_dword(instruction, 9)?)
            }
            0x0202 => Handler15TargetPayload::ByPlace {
                first: self.resolve_handler15_dword(instruction, 9)?,
                second: self.resolve_handler15_dword(instruction, 10)?,
            },
            0x0203 => Handler15TargetPayload::ByType(self.resolve_handler15_dword(instruction, 9)?),
            0x0204 => {
                Handler15TargetPayload::NotDefined(self.resolve_handler15_dword(instruction, 9)?)
            }
            0x0205 => Handler15TargetPayload::ByName(self.resolve_handler15_dword(instruction, 9)?),
            mode => return Err(Handler15ResolveError::UnsupportedTargetMode(mode)),
        };
        Ok(Handler15Invocation {
            word_0,
            word_1,
            word_2,
            word_3,
            scalar_4,
            scalar_5,
            scalar_6,
            scalar_7,
            target,
        })
    }

    fn resolve_handler15_dword(
        &self,
        instruction: &ScriptInstruction,
        position: usize,
    ) -> Result<u32, Handler15ResolveError> {
        match self.resolve_handler15_declaration(instruction, position)? {
            VarSetDeclaration {
                type_name: VarSetType::Dword,
                default_value: VarSetDefault::Dword(value),
                ..
            } => Ok(*value),
            declaration => Err(Handler15ResolveError::UnexpectedType {
                position,
                expected: VarSetType::Dword,
                found: declaration.type_name,
            }),
        }
    }

    fn resolve_handler15_float(
        &self,
        instruction: &ScriptInstruction,
        position: usize,
    ) -> Result<f32, Handler15ResolveError> {
        match self.resolve_handler15_declaration(instruction, position)? {
            VarSetDeclaration {
                type_name: VarSetType::Float,
                default_value: VarSetDefault::FloatBits(bits),
                ..
            } => Ok(f32::from_bits(*bits)),
            declaration => Err(Handler15ResolveError::UnexpectedType {
                position,
                expected: VarSetType::Float,
                found: declaration.type_name,
            }),
        }
    }

    fn resolve_handler15_declaration(
        &self,
        instruction: &ScriptInstruction,
        position: usize,
    ) -> Result<&VarSetDeclaration, Handler15ResolveError> {
        let index = *instruction
            .references
            .get(position)
            .ok_or(Handler15ResolveError::MissingReference { position })?;
        self.declarations
            .get(index as usize)
            .ok_or(Handler15ResolveError::VarSetIndexOutOfBounds {
                index,
                declarations: self.declarations.len(),
            })
    }
}

impl VarSetDefault {
    /// Returns the float value when this is a `float` default.
    #[must_use]
    pub fn as_float(self) -> Option<f32> {
        match self {
            Self::FloatBits(bits) => Some(f32::from_bits(bits)),
            Self::Dword(_) => None,
        }
    }

    /// Returns the integer value when this is a `DWORD` default.
    #[must_use]
    pub fn as_dword(self) -> Option<u32> {
        match self {
            Self::FloatBits(_) => None,
            Self::Dword(value) => Some(value),
        }
    }
}

/// Error while parsing the documented `varset.var` declaration subset.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VarSetError {
    /// A line beginning with `VAR(` did not have a closing parenthesis.
    UnterminatedDeclaration {
        /// One-based source line containing the declaration.
        line: usize,
    },
    /// A declaration omitted one of its first three fields.
    MissingField {
        /// One-based source line containing the declaration.
        line: usize,
    },
    /// A declaration type is not one of the observed GOG numeric types.
    UnsupportedType {
        /// One-based source line containing the declaration.
        line: usize,
    },
    /// A declaration name or numeric literal was not ASCII text.
    NonAsciiField {
        /// One-based source line containing the declaration.
        line: usize,
    },
    /// A `float` default was not a finite Rust-compatible decimal literal.
    InvalidFloat {
        /// One-based source line containing the declaration.
        line: usize,
    },
    /// A `DWORD` default was neither a decimal nor a hexadecimal `u32`.
    InvalidDword {
        /// One-based source line containing the declaration.
        line: usize,
    },
    /// The input exceeded the bounded declaration count.
    TooManyDeclarations {
        /// Maximum accepted declaration count.
        limit: usize,
    },
}

impl std::fmt::Display for VarSetError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnterminatedDeclaration { line } => {
                write!(formatter, "unterminated VAR declaration at line {line}")
            }
            Self::MissingField { line } => write!(formatter, "missing VAR field at line {line}"),
            Self::UnsupportedType { line } => {
                write!(formatter, "unsupported VAR type at line {line}")
            }
            Self::NonAsciiField { line } => write!(formatter, "non-ASCII VAR field at line {line}"),
            Self::InvalidFloat { line } => {
                write!(formatter, "invalid float default at line {line}")
            }
            Self::InvalidDword { line } => {
                write!(formatter, "invalid DWORD default at line {line}")
            }
            Self::TooManyDeclarations { limit } => {
                write!(formatter, "VAR declaration count exceeds limit {limit}")
            }
        }
    }
}

impl std::error::Error for VarSetError {}

/// A failure evaluating one row as a single native numeric literal.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FormulaLiteralError {
    /// The referenced ordinal was outside the preserved table.
    IndexOutOfBounds {
        /// Zero-based row index.
        index: u32,
        /// Number of accepted rows.
        rows: usize,
    },
    /// The row's expression was not one numeric token.
    UnsupportedExpression {
        /// Zero-based row index.
        index: u32,
        /// Raw expression bytes.
        expression: Vec<u8>,
    },
}

impl std::fmt::Display for FormulaLiteralError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::IndexOutOfBounds { index, rows } => {
                write!(formatter, "formula index {index} is outside {rows} rows")
            }
            Self::UnsupportedExpression { index, expression } => write!(
                formatter,
                "formula {index} expression {:?} is not one numeric literal",
                String::from_utf8_lossy(expression)
            ),
        }
    }
}

impl std::error::Error for FormulaLiteralError {}

impl FormulaTable {
    /// Parses native `FUNCTION` rows without compiling their expressions.
    ///
    /// Input is read as text: CRLF becomes LF, Ctrl-Z ends input, and each
    /// 255-byte read chunk is parsed independently. Accepted rows stay in
    /// source order so formula references retain their original ordinals.
    pub fn parse(bytes: &[u8]) -> Self {
        let mut text = Vec::with_capacity(bytes.len());
        let mut offset = 0;
        while let Some(&byte) = bytes.get(offset) {
            if byte == 0x1a {
                break;
            }
            if byte == b'\r' && bytes.get(offset + 1) == Some(&b'\n') {
                text.push(b'\n');
                offset += 2;
            } else {
                text.push(byte);
                offset += 1;
            }
        }

        let mut table = Self::default();
        let mut offset = 0;
        while offset < text.len() {
            let limit = (offset + 255).min(text.len());
            let end = text[offset..limit]
                .iter()
                .position(|byte| *byte == b'\n')
                .map_or(limit, |newline| offset + newline + 1);
            parse_formula_chunk(&mut table, &text[offset..end]);
            offset = end;
        }
        table
    }

    /// Evaluates a preserved row only when it is referenced by a formula assignment.
    ///
    /// The lexer behavior is bounded to the original number-token rules. The
    /// converter consumes the longest decimal prefix, converts it to `f64`,
    /// then rounds to the node's binary32 payload.
    ///
    /// # Errors
    ///
    /// Returns a typed error for an out-of-range ordinal or an expression
    /// containing more than one supported numeric token.
    pub fn evaluate_numeric_literal(&self, index: u32) -> Result<f32, FormulaLiteralError> {
        let Some(row) = self.rows.get(index as usize) else {
            return Err(FormulaLiteralError::IndexOutOfBounds {
                index,
                rows: self.rows.len(),
            });
        };
        parse_native_numeric_literal(&row.expression_raw).ok_or_else(|| {
            FormulaLiteralError::UnsupportedExpression {
                index,
                expression: row.expression_raw.clone(),
            }
        })
    }
}

fn parse_formula_chunk(table: &mut FormulaTable, chunk: &[u8]) {
    let visible_len = chunk
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(chunk.len());
    let visible = &chunk[..visible_len];
    let first_token = native_first_token(visible);
    if first_token.iter().take(2).any(|byte| *byte == b'/') {
        return;
    }
    if !visible
        .windows(b"FUNCTION".len())
        .any(|part| part == b"FUNCTION")
    {
        return;
    }
    let Some(open) = visible.iter().position(|byte| *byte == b'(') else {
        return;
    };
    let first_comma = visible.iter().position(|byte| *byte == b',');
    let second_comma = first_comma.and_then(|first| {
        visible[first + 1..]
            .iter()
            .position(|byte| *byte == b',')
            .map(|next| first + 1 + next)
    });
    let mut buffer = visible.to_vec();
    buffer.push(0);
    for byte in &mut buffer[..visible_len] {
        if *byte == b',' {
            *byte = 0;
        }
    }

    let name_start = open + 1;
    let expression_start = first_comma.map(|comma| comma + 1);
    let trailing_start = second_comma.map(|comma| comma + 1);
    let last_field_start = trailing_start.or(expression_start).unwrap_or(name_start);
    let last_field_end = c_string_end(&buffer, last_field_start);
    if let Some(close) = buffer[last_field_start..last_field_end]
        .iter()
        .rposition(|byte| *byte == b')')
    {
        buffer[last_field_start + close] = 0;
    }

    let name_field = c_string(&buffer, name_start);
    if name_field.is_empty() {
        return;
    }
    let expression_field = expression_start.map_or(&[][..], |start| c_string(&buffer, start));
    let trailing_field = trailing_start.map_or(&[][..], |start| c_string(&buffer, start));
    let name_token = native_first_token(name_field);
    let name_raw = if name_token.is_empty() {
        b"no_name".to_vec()
    } else {
        name_token.to_vec()
    };
    table.rows.push(FormulaRow {
        name_raw,
        expression_raw: expression_field.to_vec(),
        trailing_raw: trailing_field.to_vec(),
    });
}

fn c_string_end(buffer: &[u8], start: usize) -> usize {
    buffer[start..]
        .iter()
        .position(|byte| *byte == 0)
        .map_or(buffer.len(), |offset| start + offset)
}

fn c_string(buffer: &[u8], start: usize) -> &[u8] {
    &buffer[start..c_string_end(buffer, start)]
}

fn native_first_token(field: &[u8]) -> &[u8] {
    let start = field
        .iter()
        .position(|byte| *byte == 0 || *byte > 32)
        .unwrap_or(field.len());
    let end = field[start..]
        .iter()
        .position(|byte| *byte == 0 || *byte <= 32)
        .map_or(field.len(), |offset| start + offset);
    &field[start..end]
}

fn parse_native_numeric_literal(expression: &[u8]) -> Option<f32> {
    let start = expression
        .iter()
        .position(|byte| *byte == 0 || *byte > 32)
        .unwrap_or(expression.len());
    let expression = &expression[start..];
    let token_len = native_numeric_token_len(expression)?;
    if expression[token_len..]
        .iter()
        .any(|byte| *byte != 0 && *byte > 32)
    {
        return None;
    }
    let converted_len = native_decimal_prefix_len(&expression[..token_len])?;
    let prefix = std::str::from_utf8(&expression[..converted_len]).ok()?;
    prefix.parse::<f64>().ok().map(|value| value as f32)
}

fn native_numeric_token_len(expression: &[u8]) -> Option<usize> {
    if !expression.first().is_some_and(u8::is_ascii_digit) {
        return None;
    }
    let mut index = 0;
    let mut dot_allowed = true;
    let mut exponent_sign_allowed = false;
    while let Some(&byte) = expression.get(index) {
        if byte.is_ascii_digit() {
            exponent_sign_allowed = false;
            index += 1;
        } else if byte == b'.' && dot_allowed {
            dot_allowed = false;
            exponent_sign_allowed = false;
            index += 1;
        } else if matches!(byte, b'e' | b'E') {
            dot_allowed = true;
            exponent_sign_allowed = true;
            index += 1;
        } else if matches!(byte, b'+' | b'-') && exponent_sign_allowed {
            exponent_sign_allowed = false;
            index += 1;
        } else {
            break;
        }
    }
    Some(index)
}

fn native_decimal_prefix_len(token: &[u8]) -> Option<usize> {
    let mut index = 0;
    while token.get(index).is_some_and(u8::is_ascii_digit) {
        index += 1;
    }
    if token.get(index) == Some(&b'.') {
        index += 1;
        while token.get(index).is_some_and(u8::is_ascii_digit) {
            index += 1;
        }
    }
    if index == 0 {
        return None;
    }

    if token
        .get(index)
        .is_some_and(|byte| matches!(byte, b'e' | b'E'))
    {
        let exponent_start = index;
        index += 1;
        if token
            .get(index)
            .is_some_and(|byte| matches!(byte, b'+' | b'-'))
        {
            index += 1;
        }
        let digits_start = index;
        while token.get(index).is_some_and(u8::is_ascii_digit) {
            index += 1;
        }
        if digits_start == index {
            index = exponent_start;
        }
    }
    Some(index)
}

fn parse_init_formula_expression(
    expression: &[u8],
    declarations: &[VarSetDeclaration],
    values: &[VarSetDefault],
) -> Option<f32> {
    let (left, left_end) = parse_init_formula_atom(expression, 0, declarations, values)?;
    let operator_index = skip_native_formula_space(expression, left_end);
    let Some(&operator @ (b'+' | b'-')) = expression.get(operator_index) else {
        return expression[left_end..]
            .iter()
            .all(|byte| *byte == 0 || *byte <= 32)
            .then_some(left);
    };
    let (right, right_end) =
        parse_init_formula_atom(expression, operator_index + 1, declarations, values)?;
    if !expression[right_end..]
        .iter()
        .all(|byte| *byte == 0 || *byte <= 32)
    {
        return None;
    }
    Some(match operator {
        b'+' => right + left,
        b'-' => left - right,
        _ => unreachable!(),
    })
}

fn parse_init_formula_atom(
    expression: &[u8],
    start: usize,
    declarations: &[VarSetDeclaration],
    values: &[VarSetDefault],
) -> Option<(f32, usize)> {
    let start = skip_native_formula_space(expression, start);
    let remaining = expression.get(start..)?;
    let first = *remaining.first()?;
    let token_len = if first.is_ascii_digit() {
        native_numeric_token_len(remaining)?
    } else {
        native_variable_token_len(remaining)?
    };
    let token = &remaining[..token_len];
    if is_native_operator_atom(token) {
        return None;
    }

    let value = if let Some(index) = declarations
        .iter()
        .position(|declaration| declaration.name.as_bytes() == token)
    {
        let declaration = &declarations[index];
        let value = *values.get(index)?;
        varset_value_as_f32(declaration.type_name, value)?
    } else {
        parse_native_numeric_literal(token)?
    };
    Some((value, start + token_len))
}

fn native_variable_token_len(token: &[u8]) -> Option<usize> {
    let first = *token.first()?;
    if first == b'^' || !is_native_variable_byte(first) {
        return None;
    }
    Some(
        token
            .iter()
            .take_while(|byte| is_native_variable_byte(**byte))
            .count(),
    )
}

fn is_native_variable_byte(byte: u8) -> bool {
    byte.is_ascii() && ((byte > b'@' && byte < b'{') || byte.is_ascii_digit() || byte == b'_')
}

fn is_native_operator_atom(token: &[u8]) -> bool {
    matches!(
        token,
        b"+" | b"-" | b"*" | b"/" | b"^" | b"&" | b"|" | b"!" | b"A" | b"B" | b"N" | b"S"
    )
}

fn skip_native_formula_space(expression: &[u8], start: usize) -> usize {
    expression[start..]
        .iter()
        .position(|byte| *byte == 0 || *byte > 32)
        .map_or(expression.len(), |offset| start + offset)
}

fn varset_value_as_f32(type_name: VarSetType, value: VarSetDefault) -> Option<f32> {
    match (type_name, value) {
        (VarSetType::Float, VarSetDefault::FloatBits(bits)) => Some(f32::from_bits(bits)),
        (VarSetType::Dword, VarSetDefault::Dword(value)) => Some(value as f32),
        _ => None,
    }
}

fn varset_value_matches_type(type_name: VarSetType, value: VarSetDefault) -> bool {
    matches!(
        (type_name, value),
        (VarSetType::Float, VarSetDefault::FloatBits(_))
            | (VarSetType::Dword, VarSetDefault::Dword(_))
    )
}

fn formula_dword_value(value: f32) -> Option<u32> {
    const SIGNED_QWORD_LIMIT: f64 = 9_223_372_036_854_775_808.0;
    let value = f64::from(value);
    if !value.is_finite() || value < -SIGNED_QWORD_LIMIT || value >= SIGNED_QWORD_LIMIT {
        return None;
    }
    Some(value.trunc() as i64 as u32)
}

/// A compiled `.scr` package.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScriptPackage {
    /// Number of opcode handlers expected by the package.
    pub opcode_handler_count: u32,
    /// Named events in original file order.
    pub events: Vec<ScriptEvent>,
    /// Bytes not consumed by the recovered framing.
    pub trailing_bytes: Vec<u8>,
    /// Original package bytes.
    pub raw: Arc<[u8]>,
}

/// One named event record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScriptEvent {
    /// Declared byte count excluding the NUL terminator.
    pub name_len: u32,
    /// Name bytes including its mandatory NUL terminator.
    pub name_raw: Vec<u8>,
    /// Opaque event word following the name.
    pub event_word: u32,
    /// Nested instruction records in original file order.
    pub instructions: Vec<ScriptInstruction>,
}

/// A lossless instruction record.
///
/// Seven header words are retained in their on-disk order. The sixth word
/// declares the number of following references; the seventh follows them.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScriptInstruction {
    /// Opaque header words in original file order.
    pub header_words: [u32; INSTRUCTION_WORDS],
    /// References stored after header word five and before word six.
    pub references: Vec<u32>,
}

/// The recovered selector for an instruction's installed handler table.
///
/// The GOG AI loader copies 73 function pointers in order. Across all checked
/// GOG packages the first disk word is either one of these indices or the
/// explicit `u32::MAX` sentinel. This is a disassembly contract, not an
/// instruction executor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScriptDispatchSelector {
    /// One of the ordered handlers installed by `ai.dll`.
    Handler(u8),
    /// The on-disk `0xffff_ffff` sentinel.
    Sentinel,
    /// A value not yet observed or accepted by the GOG handler table.
    Unknown(u32),
}

/// Raw inputs resolved by the corpus-reachable `Handler(2)` before it reaches
/// the original event-record scheduler.
///
/// The field names preserve handler slot order, not guessed gameplay meaning.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Handler2RecordInput {
    /// Resolved slot 0 word.
    pub word_0: u32,
    /// Resolved slot 1 scalar.
    pub scalar_1: f32,
    /// Resolved slot 2 word.
    pub word_2: u32,
    /// Resolved slot 3 word.
    pub word_3: u32,
    /// Resolved slot 4 scalar.
    pub scalar_4: f32,
    /// Resolved slot 5 scalar.
    pub scalar_5: f32,
    /// Resolved slot 6 scalar.
    pub scalar_6: f32,
}

/// The exact three-word identity used by the original `Handler(2)` scheduler.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Handler2RecordKey {
    /// First identity word from resolved slot 0.
    pub word_0: u32,
    /// IEEE-754 bits of resolved slot 4.
    pub scalar_4_bits: u32,
    /// IEEE-754 bits of resolved slot 5.
    pub scalar_5_bits: u32,
}

impl From<Handler2RecordInput> for Handler2RecordKey {
    fn from(input: Handler2RecordInput) -> Self {
        Self {
            word_0: input.word_0,
            scalar_4_bits: input.scalar_4.to_bits(),
            scalar_5_bits: input.scalar_5.to_bits(),
        }
    }
}

/// A single backend-neutral event record created by `Handler(2)`.
///
/// This mirrors only the fields whose construction and update rules are
/// statically recovered. Event-name lookup and the downstream consumer remain
/// separate runtime work.
#[derive(Clone, Debug, PartialEq)]
pub struct Handler2Record {
    /// The three-word scheduler identity.
    pub key: Handler2RecordKey,
    /// Resolved slot 1 scalar.
    pub scalar_1: f32,
    /// Current counter word stored at original record offset `+0x24`.
    pub counter: u32,
    /// Original record offset `+0x28`: slot 2 captured when the record was created.
    increment: u32,
    /// Resolved slot 3 word.
    pub word_3: u32,
    /// Resolved slot 6 scalar.
    pub scalar_6: f32,
}

/// The result of submitting one resolved `Handler(2)` record.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Handler2RecordUpdate {
    /// Stable record position in insertion order.
    pub index: usize,
    /// Whether a new record was created.
    pub created: bool,
    /// Whether an existing record took the original refresh path.
    pub refreshed: bool,
}

/// Deterministic model of the original `Handler(2)` event-record collection.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Handler2RecordScheduler {
    records: Vec<Handler2Record>,
}

impl Handler2RecordScheduler {
    /// Returns event records in original insertion order.
    #[must_use]
    pub fn records(&self) -> &[Handler2Record] {
        &self.records
    }

    /// Inserts or refreshes one resolved handler input.
    ///
    /// The original compares the three identity words bit-for-bit. On an
    /// existing key, it refreshes only when `scalar_1` compares unequal; that
    /// update replaces `scalar_1` and `scalar_6`, then adds the slot-2 word
    /// captured when that record was created, with x86 wrapping arithmetic.
    pub fn submit(&mut self, input: Handler2RecordInput) -> Handler2RecordUpdate {
        let key = Handler2RecordKey::from(input);
        if let Some((index, record)) = self
            .records
            .iter_mut()
            .enumerate()
            .find(|(_, record)| record.key == key)
        {
            if !handler_two_scalar_equal(record.scalar_1, input.scalar_1) {
                record.scalar_1 = input.scalar_1;
                record.scalar_6 = input.scalar_6;
                record.counter = record.counter.wrapping_add(record.increment);
                return Handler2RecordUpdate {
                    index,
                    created: false,
                    refreshed: true,
                };
            }
            return Handler2RecordUpdate {
                index,
                created: false,
                refreshed: false,
            };
        }
        let index = self.records.len();
        self.records.push(Handler2Record {
            key,
            scalar_1: input.scalar_1,
            counter: input.word_2,
            increment: input.word_2,
            word_3: input.word_3,
            scalar_6: input.scalar_6,
        });
        Handler2RecordUpdate {
            index,
            created: true,
            refreshed: false,
        }
    }
}

fn handler_two_scalar_equal(left: f32, right: f32) -> bool {
    if left.is_nan() || right.is_nan() {
        return false;
    }
    let left_bits = left.to_bits();
    let right_bits = right.to_bits();
    left_bits == right_bits || (is_f32_zero_bits(left_bits) && is_f32_zero_bits(right_bits))
}

fn is_f32_zero_bits(bits: u32) -> bool {
    matches!(bits, 0 | 0x8000_0000)
}

/// Parses the numeric `VAR(...)` declarations from a legacy `varset.var` file.
///
/// Parsing is line-oriented and byte-safe: non-UTF-8 comments remain opaque,
/// while the declaration head, type, name, and default value must be ASCII.
/// `STRING(...)`, `FUNCTION(...)`, and all other non-`VAR` lines remain outside
/// this recovered numeric-default contract.
///
/// # Errors
///
/// Returns a typed error for malformed supported declarations or inputs above
/// the fixed declaration limit.
pub fn parse_varset(bytes: &[u8]) -> Result<VarSet, VarSetError> {
    let mut declarations = Vec::new();
    for (line_index, raw_line) in bytes.split(|byte| *byte == b'\n').enumerate() {
        let line = trim_ascii(strip_line_comment(raw_line));
        if !line.starts_with(b"VAR(") {
            continue;
        }
        if declarations.len() == MAX_VARSET_DECLARATIONS {
            return Err(VarSetError::TooManyDeclarations {
                limit: MAX_VARSET_DECLARATIONS,
            });
        }
        declarations.push(parse_varset_declaration(line, line_index + 1)?);
    }
    Ok(VarSet { declarations })
}

fn strip_line_comment(line: &[u8]) -> &[u8] {
    line.windows(2)
        .position(|window| window == b"//")
        .map_or(line, |index| &line[..index])
}

fn trim_ascii(bytes: &[u8]) -> &[u8] {
    let start = bytes
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())
        .unwrap_or(bytes.len());
    let end = bytes
        .iter()
        .rposition(|byte| !byte.is_ascii_whitespace())
        .map_or(start, |index| index + 1);
    &bytes[start..end]
}

fn parse_varset_declaration(
    line: &[u8],
    line_number: usize,
) -> Result<VarSetDeclaration, VarSetError> {
    let declaration = line
        .strip_prefix(b"VAR(")
        .and_then(|body| body.strip_suffix(b")"))
        .or_else(|| {
            line.strip_prefix(b"VAR(")
                .and_then(|body| body.strip_suffix(b");"))
        })
        .ok_or(VarSetError::UnterminatedDeclaration { line: line_number })?;
    let mut fields = declaration.split(|byte| *byte == b',').map(trim_ascii);
    let type_raw = fields
        .next()
        .filter(|field| !field.is_empty())
        .ok_or(VarSetError::MissingField { line: line_number })?;
    let name_raw = fields
        .next()
        .filter(|field| !field.is_empty())
        .ok_or(VarSetError::MissingField { line: line_number })?;
    let default_raw = fields
        .next()
        .filter(|field| !field.is_empty())
        .ok_or(VarSetError::MissingField { line: line_number })?;
    let type_text = std::str::from_utf8(type_raw)
        .map_err(|_| VarSetError::NonAsciiField { line: line_number })?;
    let name = std::str::from_utf8(name_raw)
        .map_err(|_| VarSetError::NonAsciiField { line: line_number })?
        .to_owned();
    let default_text = std::str::from_utf8(default_raw)
        .map_err(|_| VarSetError::NonAsciiField { line: line_number })?;
    let (type_name, default_value) = match type_text {
        "float" => {
            let value = default_text
                .parse::<f32>()
                .ok()
                .filter(|value| value.is_finite())
                .ok_or(VarSetError::InvalidFloat { line: line_number })?;
            (VarSetType::Float, VarSetDefault::FloatBits(value.to_bits()))
        }
        "DWORD" => (
            VarSetType::Dword,
            VarSetDefault::Dword(parse_varset_dword(default_text, line_number)?),
        ),
        _ => return Err(VarSetError::UnsupportedType { line: line_number }),
    };
    Ok(VarSetDeclaration {
        type_name,
        name,
        default_value,
    })
}

fn parse_varset_dword(value: &str, line: usize) -> Result<u32, VarSetError> {
    let (radix, digits) = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
        .map_or((10, value), |digits| (16, digits));
    u32::from_str_radix(digits, radix).map_err(|_| VarSetError::InvalidDword { line })
}

impl ScriptInstruction {
    /// Returns the recovered dispatch selector from the first disk word.
    #[must_use]
    pub fn dispatch_selector(&self) -> ScriptDispatchSelector {
        match self.header_words[0] {
            value if value < GOG_HANDLER_COUNT => {
                ScriptDispatchSelector::Handler(self.header_words[0] as u8)
            }
            u32::MAX => ScriptDispatchSelector::Sentinel,
            value => ScriptDispatchSelector::Unknown(value),
        }
    }
}

/// Decodes a compiled AI package using default safety limits.
///
/// # Errors
///
/// Returns a bounded [`DecodeError`] on truncated or oversized input.
pub fn decode(bytes: &[u8]) -> Result<ScriptPackage, DecodeError> {
    decode_with_limits(bytes, Limits::default())
}

/// Decodes a compiled AI package using explicit safety limits.
///
/// # Errors
///
/// Returns a bounded [`DecodeError`] on truncated or oversized input.
pub fn decode_with_limits(bytes: &[u8], limits: Limits) -> Result<ScriptPackage, DecodeError> {
    if u64::try_from(bytes.len()).map_err(|_| DecodeError::IntegerOverflow)? > limits.max_file_bytes
    {
        return Err(DecodeError::LimitExceeded {
            count: u64::try_from(bytes.len()).map_err(|_| DecodeError::IntegerOverflow)?,
            limit: limits.max_file_bytes,
        });
    }
    let mut cursor = Cursor::new(bytes);
    let opcode_handler_count = cursor.read_u32_le()?;
    let event_count = cursor.read_u32_le()?;
    checked_allocation_len(u64::from(event_count), u64::from(limits.max_entries))?;
    let mut events =
        Vec::with_capacity(usize::try_from(event_count).map_err(|_| DecodeError::IntegerOverflow)?);
    for _ in 0..event_count {
        events.push(read_event(&mut cursor, limits)?);
    }
    let trailing_bytes = cursor.read_exact(cursor.remaining())?.to_vec();
    Ok(ScriptPackage {
        opcode_handler_count,
        events,
        trailing_bytes,
        raw: Arc::from(bytes),
    })
}

fn read_event(cursor: &mut Cursor<'_>, limits: Limits) -> Result<ScriptEvent, DecodeError> {
    let name_len = cursor.read_u32_le()?;
    let name_bytes = u64::from(name_len)
        .checked_add(1)
        .ok_or(DecodeError::IntegerOverflow)?;
    let name_len_usize = checked_allocation_len(name_bytes, u64::from(limits.max_string_bytes))?;
    let name_raw = cursor.read_exact(name_len_usize)?.to_vec();
    if name_raw.last().copied() != Some(0) {
        return Err(DecodeError::Invalid(
            "script event name is not NUL terminated",
        ));
    }
    let event_word = cursor.read_u32_le()?;
    let instruction_count = cursor.read_u32_le()?;
    checked_allocation_len(u64::from(instruction_count), u64::from(limits.max_entries))?;
    let minimum = u64::from(instruction_count)
        .checked_mul(INSTRUCTION_HEADER_BYTES)
        .ok_or(DecodeError::IntegerOverflow)?;
    if minimum > u64::try_from(cursor.remaining()).map_err(|_| DecodeError::IntegerOverflow)? {
        return Err(DecodeError::UnexpectedEof {
            offset: cursor.offset(),
            needed: minimum,
            remaining: u64::try_from(cursor.remaining())
                .map_err(|_| DecodeError::IntegerOverflow)?,
        });
    }
    let mut instructions = Vec::with_capacity(
        usize::try_from(instruction_count).map_err(|_| DecodeError::IntegerOverflow)?,
    );
    for _ in 0..instruction_count {
        instructions.push(read_instruction(cursor, limits)?);
    }
    Ok(ScriptEvent {
        name_len,
        name_raw,
        event_word,
        instructions,
    })
}

fn read_instruction(
    cursor: &mut Cursor<'_>,
    limits: Limits,
) -> Result<ScriptInstruction, DecodeError> {
    let mut header_words = [0; INSTRUCTION_WORDS];
    for word in &mut header_words[..5] {
        *word = cursor.read_u32_le()?;
    }
    header_words[5] = cursor.read_u32_le()?;
    let reference_count = header_words[5];
    let reference_bytes = u64::from(reference_count)
        .checked_mul(4)
        .ok_or(DecodeError::IntegerOverflow)?;
    if reference_bytes
        > u64::try_from(cursor.remaining()).map_err(|_| DecodeError::IntegerOverflow)?
    {
        return Err(DecodeError::UnexpectedEof {
            offset: cursor.offset(),
            needed: reference_bytes,
            remaining: u64::try_from(cursor.remaining())
                .map_err(|_| DecodeError::IntegerOverflow)?,
        });
    }
    checked_allocation_len(
        u64::from(reference_count),
        u64::from(limits.max_array_items),
    )?;
    let mut references = Vec::with_capacity(
        usize::try_from(reference_count).map_err(|_| DecodeError::IntegerOverflow)?,
    );
    for _ in 0..reference_count {
        references.push(cursor.read_u32_le()?);
    }
    header_words[6] = cursor.read_u32_le()?;
    Ok(ScriptInstruction {
        header_words,
        references,
    })
}

#[cfg(test)]
mod tests {
    use super::{
        decode, decode_with_limits, parse_varset, FormulaTable, Handler15ResolveError,
        Handler15TargetPayload, Handler19DwordWrite, Handler19InitInput, Handler19ResolveError,
        Handler2RecordInput, Handler2RecordScheduler, Handler30ResolveError, Handler8Reset,
        Handler8ResolveError, ScriptDispatchSelector, ScriptEvent, ScriptInitResolution,
        ScriptInitResolveError, ScriptInitWrite, ScriptInstruction, ScriptPackage, VarSetDefault,
        VarSetError, VarSetType, VmHostCallbackCommand, GOG_HANDLER_COUNT, INSTRUCTION_WORDS,
    };
    use fparkan_binary::{DecodeError, Limits};

    fn init_package(instructions: Vec<ScriptInstruction>) -> ScriptPackage {
        ScriptPackage {
            opcode_handler_count: 73,
            events: vec![ScriptEvent {
                name_len: 4,
                name_raw: b"Init\0".to_vec(),
                event_word: 0,
                instructions,
            }],
            trailing_bytes: Vec::new(),
            raw: std::sync::Arc::from(&b""[..]),
        }
    }

    fn init_formula_assignment(target: u32, formula: u32) -> ScriptInstruction {
        ScriptInstruction {
            header_words: [u32::MAX, target, u32::MAX, u32::MAX, 6, 0, formula],
            references: Vec::new(),
        }
    }

    fn init_handler19() -> ScriptInstruction {
        ScriptInstruction {
            header_words: [19, u32::MAX, u32::MAX, u32::MAX, 6, 3, u32::MAX],
            references: vec![0, 1, 2],
        }
    }

    fn init_input() -> Handler19InitInput {
        Handler19InitInput {
            first_x87_word: 10,
            second_x87_word: 20,
            third_word: 30,
        }
    }

    fn one_formula(expression: &[u8]) -> FormulaTable {
        let mut source = b"FUNCTION(test, ".to_vec();
        source.extend_from_slice(expression);
        source.extend_from_slice(b", )\n");
        FormulaTable::parse(&source)
    }

    fn handler_two_input(
        scalar_1: f32,
        scalar_4: f32,
        scalar_5: f32,
        scalar_6: f32,
        word_2: u32,
    ) -> Handler2RecordInput {
        Handler2RecordInput {
            word_0: 7,
            scalar_1,
            word_2,
            word_3: 11,
            scalar_4,
            scalar_5,
            scalar_6,
        }
    }

    #[test]
    fn handler_two_scheduler_uses_three_word_bit_identity_and_refresh_contract() {
        let mut scheduler = Handler2RecordScheduler::default();
        let first = handler_two_input(1.5, -0.0, 3.0, 9.0, 4);
        assert_eq!(
            scheduler.submit(first),
            super::Handler2RecordUpdate {
                index: 0,
                created: true,
                refreshed: false,
            }
        );
        assert_eq!(scheduler.records().len(), 1);
        assert_eq!(scheduler.records()[0].counter, 4);

        let unchanged = handler_two_input(1.5, -0.0, 3.0, 12.0, 99);
        assert_eq!(
            scheduler.submit(unchanged),
            super::Handler2RecordUpdate {
                index: 0,
                created: false,
                refreshed: false,
            }
        );
        assert_eq!(scheduler.records()[0].counter, 4);
        assert_eq!(scheduler.records()[0].scalar_6.to_bits(), 9.0_f32.to_bits());

        let refreshed = handler_two_input(2.5, -0.0, 3.0, 12.0, 99);
        assert_eq!(
            scheduler.submit(refreshed),
            super::Handler2RecordUpdate {
                index: 0,
                created: false,
                refreshed: true,
            }
        );
        assert_eq!(scheduler.records()[0].counter, 8);
        assert_eq!(
            scheduler.records()[0].scalar_6.to_bits(),
            12.0_f32.to_bits()
        );

        let refreshed_again = handler_two_input(3.5, -0.0, 3.0, 13.0, 7);
        assert!(scheduler.submit(refreshed_again).refreshed);
        assert_eq!(scheduler.records()[0].counter, 12);
        assert_eq!(
            scheduler.records()[0].scalar_6.to_bits(),
            13.0_f32.to_bits()
        );

        let positive_zero_key = handler_two_input(2.5, 0.0, 3.0, 12.0, 1);
        assert_eq!(scheduler.submit(positive_zero_key).index, 1);
        assert_eq!(scheduler.records().len(), 2);
    }

    #[test]
    fn handler_two_scheduler_refreshes_nan_and_wraps_counter() {
        let mut scheduler = Handler2RecordScheduler::default();
        scheduler.submit(handler_two_input(f32::NAN, 1.0, 2.0, 3.0, u32::MAX));
        let update = scheduler.submit(handler_two_input(f32::NAN, 1.0, 2.0, 4.0, 2));
        assert_eq!(update.index, 0);
        assert!(update.refreshed);
        assert_eq!(scheduler.records()[0].counter, u32::MAX - 1);
        assert_eq!(scheduler.records()[0].scalar_6.to_bits(), 4.0_f32.to_bits());

        let update = scheduler.submit(handler_two_input(f32::NAN, 1.0, 2.0, 5.0, 17));
        assert!(update.refreshed);
        assert_eq!(scheduler.records()[0].counter, u32::MAX - 2);
        assert_eq!(scheduler.records()[0].scalar_6.to_bits(), 5.0_f32.to_bits());
    }

    #[test]
    fn handler_two_scheduler_treats_signed_zero_value_as_unchanged() {
        let mut scheduler = Handler2RecordScheduler::default();
        scheduler.submit(handler_two_input(-0.0, 1.0, 2.0, 3.0, 5));
        let update = scheduler.submit(handler_two_input(0.0, 1.0, 2.0, 4.0, 9));
        assert!(!update.created);
        assert!(!update.refreshed);
        assert_eq!(scheduler.records()[0].counter, 5);
        assert_eq!(scheduler.records()[0].scalar_6.to_bits(), 3.0_f32.to_bits());
    }

    #[test]
    fn varset_parser_preserves_typed_defaults_and_legacy_comment_bytes() {
        let source = b"// \xff legacy comment\r\n\
VAR( float, fDifficulty, 0.5) // ignored\r\n\
VAR( DWORD, CLASS_BUILDING, 0x80000000);\r\n\
STRING( 8, ignored, ignored, ignored)\r\n";
        let parsed = parse_varset(source).expect("valid varset declarations");
        assert_eq!(parsed.declarations.len(), 2);
        assert_eq!(parsed.declarations[0].type_name, VarSetType::Float);
        assert_eq!(parsed.declarations[0].name, "fDifficulty");
        assert_eq!(
            parsed.declarations[0].default_value,
            VarSetDefault::FloatBits(0.5_f32.to_bits())
        );
        assert_eq!(parsed.declarations[1].type_name, VarSetType::Dword);
        assert_eq!(parsed.declarations[1].name, "CLASS_BUILDING");
        assert_eq!(
            parsed.declarations[1].default_value.as_dword(),
            Some(0x8000_0000)
        );
        assert_eq!(parsed.declarations[0].default_value.as_float(), Some(0.5));
    }

    #[test]
    fn varset_parser_rejects_malformed_supported_declarations() {
        assert_eq!(
            parse_varset(b"VAR( float, f0, nope)\n"),
            Err(VarSetError::InvalidFloat { line: 1 })
        );
        assert_eq!(
            parse_varset(b"VAR( BYTE, b0, 1)\n"),
            Err(VarSetError::UnsupportedType { line: 1 })
        );
        assert_eq!(
            parse_varset(b"VAR( DWORD, d0, 0x100000000)\n"),
            Err(VarSetError::InvalidDword { line: 1 })
        );
        assert_eq!(
            parse_varset(b"VAR( DWORD, d0, 1\n"),
            Err(VarSetError::UnterminatedDeclaration { line: 1 })
        );
    }

    #[test]
    fn formula_table_preserves_native_rows_and_field_defaults() {
        let table = FormulaTable::parse(
            b"// a comment\r\nFUNCTION(no_fields)\r\nFUNCTION(name,0)\r\nFUNCTION(name,0,)\r\nFUNCTION(   , 2, )\r\nFUNCTION(, 3,)\r\nFUNCTION missing_parenthesis,4,\r\nFUNCTION()\r\n/x FUNCTION(ignored,5,)\r\nx/FUNCTION(ignored,6,)\r\nx,FUNCTION(name,0,)\r\nx,FUNCTION(name)\r\n",
        );

        assert_eq!(table.rows.len(), 6);
        assert_eq!(table.rows[0].name_raw, b"no_fields");
        assert!(table.rows[0].expression_raw.is_empty());
        assert_eq!(table.rows[1].name_raw, b"name");
        assert_eq!(table.rows[1].expression_raw, b"0");
        assert!(table.rows[1].trailing_raw.is_empty());
        assert_eq!(table.rows[2].expression_raw, b"0");
        assert!(table.rows[2].trailing_raw.is_empty());
        assert_eq!(table.rows[3].name_raw, b"no_name");
        assert_eq!(table.rows[3].expression_raw, b" 2");
        assert_eq!(table.rows[4].name_raw, b"name");
        assert_eq!(table.rows[4].expression_raw, b"FUNCTION(name");
        assert_eq!(table.rows[4].trailing_raw, b"0");
        assert_eq!(table.rows[5].name_raw, b"name");
        assert_eq!(table.rows[5].expression_raw, b"FUNCTION(name");
        assert!(table.rows[5].trailing_raw.is_empty());
    }

    #[test]
    fn formula_table_chunks_without_reassembling_and_stops_at_text_eof() {
        let mut source = vec![b' '; 255];
        source.extend_from_slice(b"FUNCTION(before, 1, )\r\n");
        source.extend_from_slice(b"FUNCTION(after, 2, )\r\n");
        source.push(0x1a);
        source.extend_from_slice(b"FUNCTION(ignored, 3, )\r\n");

        let table = FormulaTable::parse(&source);
        assert_eq!(table.rows.len(), 2);
        assert_eq!(table.rows[0].name_raw, b"before");
        assert_eq!(table.rows[1].name_raw, b"after");
    }

    #[test]
    fn formula_table_uses_only_the_nul_terminated_view_of_each_chunk() {
        let table = FormulaTable::parse(
            b"FUNCTION(visible, 1, )\0 FUNCTION(hidden, 2, )\nFUNCTION(next, 3, )\n",
        );
        assert_eq!(table.rows.len(), 2);
        assert_eq!(table.rows[0].name_raw, b"visible");
        assert_eq!(table.rows[1].name_raw, b"next");
    }

    #[test]
    fn formula_literal_evaluation_uses_the_native_numeric_prefix() {
        let table = FormulaTable::parse(
            b"FUNCTION(scientific,1e-2,)\nFUNCTION(integer_dot,1.,)\nFUNCTION(exp_prefix,1e2.3,)\nFUNCTION(incomplete,1e+,)\nFUNCTION(unary,-1,)\nFUNCTION(operator,1+2,)\n",
        );

        for (index, expected) in [(0, 0.01_f32), (1, 1.0), (2, 100.0), (3, 1.0)] {
            assert_eq!(
                table
                    .evaluate_numeric_literal(index)
                    .expect("native numeric token"),
                expected
            );
        }
        assert!(table.evaluate_numeric_literal(4).is_err());
        assert!(table.evaluate_numeric_literal(5).is_err());
        assert!(table.evaluate_numeric_literal(6).is_err());
    }

    #[test]
    fn handler_thirty_resolves_only_proven_dword_operands() {
        let varset = parse_varset(
            b"VAR( float, f0, 0.5)\nVAR( DWORD, first, 0x12)\nVAR( DWORD, second, 9)\n",
        )
        .expect("varset");
        let instruction = ScriptInstruction {
            header_words: [30, 0, 0, 0, 0, 2, 0],
            references: vec![1, 2],
        };
        assert_eq!(
            varset.resolve_handler30(&instruction),
            Ok(VmHostCallbackCommand {
                mode: 0,
                first: 0x12,
                second: 9,
            })
        );
    }

    #[test]
    fn handler_thirty_reads_instantiated_dword_cells_instead_of_defaults() {
        let varset =
            parse_varset(b"VAR( DWORD, first, 0x12)\nVAR( DWORD, second, 9)\n").expect("varset");
        let instruction = ScriptInstruction {
            header_words: [30, 0, 0, 0, 0, 2, 0],
            references: vec![0, 1],
        };
        assert_eq!(
            varset.resolve_handler30_with_values(
                &instruction,
                &[VarSetDefault::Dword(728), VarSetDefault::Dword(449)],
            ),
            Ok(VmHostCallbackCommand {
                mode: 0,
                first: 728,
                second: 449,
            })
        );
    }

    #[test]
    fn handler_thirty_keeps_float_and_malformed_references_explicit() {
        let varset = parse_varset(b"VAR( float, f0, 0.5)\n").expect("varset");
        let float_operand = ScriptInstruction {
            header_words: [30, 0, 0, 0, 0, 2, 0],
            references: vec![0, 0],
        };
        assert_eq!(
            varset.resolve_handler30(&float_operand),
            Err(Handler30ResolveError::FloatRequiresX87 { index: 0 })
        );
        let dword_varset = parse_varset(b"VAR( DWORD, d0, 1)\n").expect("dword varset");
        let missing_operand = ScriptInstruction {
            header_words: [30, 0, 0, 0, 0, 1, 0],
            references: vec![0],
        };
        assert_eq!(
            dword_varset.resolve_handler30(&missing_operand),
            Err(Handler30ResolveError::MissingReference { position: 1 })
        );
        let out_of_range = ScriptInstruction {
            header_words: [30, 0, 0, 0, 0, 2, 0],
            references: vec![1, 1],
        };
        assert_eq!(
            dword_varset.resolve_handler30(&out_of_range),
            Err(Handler30ResolveError::VarSetIndexOutOfBounds {
                index: 1,
                declarations: 1,
            })
        );
    }

    #[test]
    fn handler_eight_resolves_dword_state_and_proven_reset_branches() {
        let varset = parse_varset(
            b"VAR( DWORD, solving, 1)\nVAR( DWORD, solved, 2)\nVAR( DWORD, state_three, 3)\n",
        )
        .expect("valid Handler(8) varset");
        for (reference, reset) in [
            (0, Handler8Reset::None),
            (1, Handler8Reset::StateTwo),
            (2, Handler8Reset::StateThree),
        ] {
            let change = varset
                .resolve_handler8(
                    &ScriptInstruction {
                        header_words: [8, 0, 0, 0, 0, 1, 0],
                        references: vec![reference],
                    },
                    17,
                )
                .expect("typed Handler(8) state");
            assert_eq!(change.record_index, 17);
            assert_eq!(change.next_state, reference + 1);
            assert_eq!(change.reset, reset);
        }
    }

    #[test]
    fn handler_eight_rejects_missing_float_and_out_of_range_operands() {
        let empty = parse_varset(b"").expect("empty varset");
        assert_eq!(
            empty.resolve_handler8(
                &ScriptInstruction {
                    header_words: [8, 0, 0, 0, 0, 0, 0],
                    references: Vec::new(),
                },
                0,
            ),
            Err(Handler8ResolveError::MissingReference)
        );
        let float = parse_varset(b"VAR( float, value, 1.0)\n").expect("float varset");
        assert_eq!(
            float.resolve_handler8(
                &ScriptInstruction {
                    header_words: [8, 0, 0, 0, 0, 1, 0],
                    references: vec![0],
                },
                0,
            ),
            Err(Handler8ResolveError::UnexpectedType {
                found: VarSetType::Float,
            })
        );
        assert_eq!(
            float.resolve_handler8(
                &ScriptInstruction {
                    header_words: [8, 0, 0, 0, 0, 1, 0],
                    references: vec![1],
                },
                0,
            ),
            Err(Handler8ResolveError::VarSetIndexOutOfBounds {
                index: 1,
                declarations: 1,
            })
        );
    }

    #[test]
    fn handler_nineteen_writes_exact_default_init_dwords_without_float_coercion() {
        let varset = parse_varset(
            b"VAR( DWORD, ClanBaseX, 950)\nVAR( DWORD, ClanBaseY, 1000)\nVAR( DWORD, ClanID, 0)\n",
        )
        .expect("default Init targets");
        let writes = varset
            .resolve_handler19(
                &ScriptInstruction {
                    header_words: [19, 0, 0, 0, 0, 3, 0],
                    references: vec![0, 1, 2],
                },
                Handler19InitInput {
                    first_x87_word: 500,
                    second_x87_word: 752,
                    third_word: 0,
                },
            )
            .expect("resolved Init writes");
        assert_eq!(
            writes,
            [
                Handler19DwordWrite {
                    index: 0,
                    value: 500
                },
                Handler19DwordWrite {
                    index: 1,
                    value: 752
                },
                Handler19DwordWrite { index: 2, value: 0 },
            ]
        );
    }

    #[test]
    fn handler_nineteen_keeps_target_errors_explicit() {
        let float = parse_varset(b"VAR( float, f0, 1.0)\n").expect("float target");
        let instruction = ScriptInstruction {
            header_words: [19, 0, 0, 0, 0, 1, 0],
            references: vec![0],
        };
        assert_eq!(
            float.resolve_handler19(
                &instruction,
                Handler19InitInput {
                    first_x87_word: 0,
                    second_x87_word: 0,
                    third_word: 0,
                },
            ),
            Err(Handler19ResolveError::UnexpectedType {
                position: 0,
                found: VarSetType::Float,
            })
        );
        let empty = parse_varset(b"").expect("empty varset");
        assert_eq!(
            empty.resolve_handler19(
                &ScriptInstruction {
                    header_words: [19, 0, 0, 0, 0, 0, 0],
                    references: Vec::new(),
                },
                Handler19InitInput {
                    first_x87_word: 0,
                    second_x87_word: 0,
                    third_word: 0,
                },
            ),
            Err(Handler19ResolveError::MissingReference { position: 0 })
        );
    }

    #[test]
    fn script_init_uses_first_exact_event_and_keeps_instruction_order() {
        let varset = parse_varset(b"VAR( DWORD, x, 0)\nVAR( DWORD, y, 0)\nVAR( DWORD, id, 0)\n")
            .expect("DWORD targets");
        let action = |references: Vec<u32>| ScriptInstruction {
            header_words: [19, u32::MAX, u32::MAX, u32::MAX, 6, 3, u32::MAX],
            references,
        };
        let package = ScriptPackage {
            opcode_handler_count: 73,
            events: vec![
                ScriptEvent {
                    name_len: 11,
                    name_raw: b"Init\0suffix\0".to_vec(),
                    event_word: 0x1234,
                    instructions: vec![action(vec![0, 1, 2]), action(vec![1, 0, 2])],
                },
                ScriptEvent {
                    name_len: 4,
                    name_raw: b"Init\0".to_vec(),
                    event_word: 0x5678,
                    instructions: vec![ScriptInstruction {
                        header_words: [18, u32::MAX, u32::MAX, 0, 0, 0, 0],
                        references: Vec::new(),
                    }],
                },
            ],
            trailing_bytes: Vec::new(),
            raw: std::sync::Arc::from(&b""[..]),
        };

        let result = varset
            .resolve_init(
                &package,
                Handler19InitInput {
                    first_x87_word: 7,
                    second_x87_word: 8,
                    third_word: 9,
                },
                None,
            )
            .expect("valid first Init event");
        assert_eq!(
            result,
            ScriptInitResolution::Executed {
                event_index: 0,
                event_word: 0x1234,
                writes: vec![
                    ScriptInitWrite {
                        index: 0,
                        value: VarSetDefault::Dword(7),
                    },
                    ScriptInitWrite {
                        index: 1,
                        value: VarSetDefault::Dword(8),
                    },
                    ScriptInitWrite {
                        index: 2,
                        value: VarSetDefault::Dword(9),
                    },
                    ScriptInitWrite {
                        index: 1,
                        value: VarSetDefault::Dword(7),
                    },
                    ScriptInitWrite {
                        index: 0,
                        value: VarSetDefault::Dword(8),
                    },
                    ScriptInitWrite {
                        index: 2,
                        value: VarSetDefault::Dword(9),
                    },
                ],
            }
        );
    }

    #[test]
    fn script_init_missing_exact_name_has_no_writes() {
        let varset = parse_varset(b"").expect("empty varset");
        let package = ScriptPackage {
            opcode_handler_count: 73,
            events: vec![
                ScriptEvent {
                    name_len: 4,
                    name_raw: b"init\0".to_vec(),
                    event_word: 1,
                    instructions: Vec::new(),
                },
                ScriptEvent {
                    name_len: 4,
                    name_raw: b"INIT\0".to_vec(),
                    event_word: 2,
                    instructions: Vec::new(),
                },
            ],
            trailing_bytes: Vec::new(),
            raw: std::sync::Arc::from(&b""[..]),
        };

        assert_eq!(
            varset.resolve_init(
                &package,
                Handler19InitInput {
                    first_x87_word: 7,
                    second_x87_word: 8,
                    third_word: 9,
                },
                None,
            ),
            Ok(ScriptInitResolution::Missing)
        );
    }

    #[test]
    fn script_init_accepts_empty_first_event_as_executed_with_no_writes() {
        let varset = parse_varset(b"").expect("empty varset");
        let package = ScriptPackage {
            opcode_handler_count: 73,
            events: vec![
                ScriptEvent {
                    name_len: 4,
                    name_raw: b"Init\0".to_vec(),
                    event_word: 0x1234,
                    instructions: Vec::new(),
                },
                ScriptEvent {
                    name_len: 4,
                    name_raw: b"Init\0".to_vec(),
                    event_word: 0x5678,
                    instructions: vec![ScriptInstruction {
                        header_words: [18, u32::MAX, u32::MAX, 0, 0, 0, 0],
                        references: Vec::new(),
                    }],
                },
            ],
            trailing_bytes: Vec::new(),
            raw: std::sync::Arc::from(&b""[..]),
        };

        assert_eq!(
            varset.resolve_init(
                &package,
                Handler19InitInput {
                    first_x87_word: 7,
                    second_x87_word: 8,
                    third_word: 9,
                },
                None,
            ),
            Ok(ScriptInitResolution::Executed {
                event_index: 0,
                event_word: 0x1234,
                writes: Vec::new(),
            })
        );
    }

    #[test]
    fn script_init_reports_unsupported_selector_and_discards_prior_writes() {
        let varset = parse_varset(b"VAR( DWORD, x, 0)\nVAR( DWORD, y, 0)\nVAR( DWORD, id, 0)\n")
            .expect("DWORD targets");
        let package = ScriptPackage {
            opcode_handler_count: 73,
            events: vec![ScriptEvent {
                name_len: 4,
                name_raw: b"Init\0".to_vec(),
                event_word: 0,
                instructions: vec![
                    ScriptInstruction {
                        header_words: [19, u32::MAX, u32::MAX, u32::MAX, 6, 3, u32::MAX],
                        references: vec![0, 1, 2],
                    },
                    ScriptInstruction {
                        header_words: [18, u32::MAX, u32::MAX, 0, 0, 0, 0],
                        references: Vec::new(),
                    },
                    ScriptInstruction {
                        header_words: [19, u32::MAX, u32::MAX, u32::MAX, 6, 3, u32::MAX],
                        references: vec![0, 1, 2],
                    },
                ],
            }],
            trailing_bytes: Vec::new(),
            raw: std::sync::Arc::from(&b""[..]),
        };

        assert_eq!(
            varset.resolve_init(
                &package,
                Handler19InitInput {
                    first_x87_word: 7,
                    second_x87_word: 8,
                    third_word: 9,
                },
                None,
            ),
            Ok(ScriptInitResolution::Unsupported {
                event_index: 0,
                instruction_index: 1,
                error: ScriptInitResolveError::UnsupportedSelector {
                    found: super::ScriptDispatchSelector::Handler(18),
                },
            })
        );
    }

    #[test]
    fn script_init_stops_at_unsupported_selector_before_any_write() {
        let varset = parse_varset(b"VAR( DWORD, x, 0)\nVAR( DWORD, y, 0)\nVAR( DWORD, id, 0)\n")
            .expect("DWORD targets");
        let package = ScriptPackage {
            opcode_handler_count: 73,
            events: vec![ScriptEvent {
                name_len: 4,
                name_raw: b"Init\0".to_vec(),
                event_word: 0,
                instructions: vec![
                    ScriptInstruction {
                        header_words: [18, u32::MAX, u32::MAX, 0, 0, 0, 0],
                        references: Vec::new(),
                    },
                    ScriptInstruction {
                        header_words: [19, u32::MAX, u32::MAX, u32::MAX, 6, 3, u32::MAX],
                        references: vec![0, 1, 2],
                    },
                ],
            }],
            trailing_bytes: Vec::new(),
            raw: std::sync::Arc::from(&b""[..]),
        };

        assert_eq!(
            varset.resolve_init(
                &package,
                Handler19InitInput {
                    first_x87_word: 7,
                    second_x87_word: 8,
                    third_word: 9,
                },
                None,
            ),
            Ok(ScriptInitResolution::Unsupported {
                event_index: 0,
                instruction_index: 0,
                error: ScriptInitResolveError::UnsupportedSelector {
                    found: super::ScriptDispatchSelector::Handler(18),
                },
            })
        );
    }

    #[test]
    fn script_init_uses_handler_nineteen_control_and_result_fields() {
        let varset = parse_varset(b"VAR( DWORD, x, 0)\nVAR( DWORD, y, 0)\nVAR( DWORD, id, 0)\n")
            .expect("DWORD targets");
        let input = Handler19InitInput {
            first_x87_word: 7,
            second_x87_word: 8,
            third_word: 9,
        };
        let resolve = |header_words| {
            let package = ScriptPackage {
                opcode_handler_count: 73,
                events: vec![ScriptEvent {
                    name_len: 4,
                    name_raw: b"Init\0".to_vec(),
                    event_word: 0,
                    instructions: vec![ScriptInstruction {
                        header_words,
                        references: vec![0, 1, 2],
                    }],
                }],
                trailing_bytes: Vec::new(),
                raw: std::sync::Arc::from(&b""[..]),
            };
            varset.resolve_init(&package, input, None)
        };

        let actual = (
            resolve([19, u32::MAX, u32::MAX, 2, 6, 3, u32::MAX]),
            resolve([19, 0x1234, u32::MAX, u32::MAX, 6, 3, u32::MAX]),
            resolve([19, u32::MAX, 0xdecafbad, u32::MAX, 6, 3, u32::MAX]),
        );
        assert_eq!(
            actual,
            (
                Ok(ScriptInitResolution::Unsupported {
                    event_index: 0,
                    instruction_index: 0,
                    error: ScriptInitResolveError::Handler19(
                        Handler19ResolveError::UnsupportedControlWord { found: 2 },
                    ),
                }),
                Ok(ScriptInitResolution::Unsupported {
                    event_index: 0,
                    instruction_index: 0,
                    error: ScriptInitResolveError::Handler19(
                        Handler19ResolveError::UnsupportedResultSave { found: 0x1234 },
                    ),
                }),
                Ok(ScriptInitResolution::Executed {
                    event_index: 0,
                    event_word: 0,
                    writes: vec![
                        ScriptInitWrite {
                            index: 0,
                            value: VarSetDefault::Dword(7),
                        },
                        ScriptInitWrite {
                            index: 1,
                            value: VarSetDefault::Dword(8),
                        },
                        ScriptInitWrite {
                            index: 2,
                            value: VarSetDefault::Dword(9),
                        },
                    ],
                }),
            )
        );
    }

    #[test]
    fn script_init_assigns_numeric_literals_using_the_target_type() {
        let varset = parse_varset(b"VAR(DWORD, integer, 0)\nVAR(float, scalar, 0)\n")
            .expect("numeric declarations");
        let package = ScriptPackage {
            opcode_handler_count: 73,
            events: vec![ScriptEvent {
                name_len: 4,
                name_raw: b"Init\0".to_vec(),
                event_word: 0,
                instructions: vec![
                    ScriptInstruction {
                        header_words: [u32::MAX, 0, u32::MAX, u32::MAX, 0xdead_beef, 0, 0],
                        references: Vec::new(),
                    },
                    ScriptInstruction {
                        header_words: [u32::MAX, 1, u32::MAX, u32::MAX, 0xdead_beef, 0, 1],
                        references: Vec::new(),
                    },
                ],
            }],
            trailing_bytes: Vec::new(),
            raw: std::sync::Arc::from(&b""[..]),
        };
        let formulas = FormulaTable::parse(
            b"FUNCTION(integer, 12.75, )\nFUNCTION(scalar, 0.8, )\nFUNCTION(unused, target + 2, )\n",
        );

        assert_eq!(
            varset.resolve_init(
                &package,
                Handler19InitInput {
                    first_x87_word: 0,
                    second_x87_word: 0,
                    third_word: 0,
                },
                Some(&formulas),
            ),
            Ok(ScriptInitResolution::Executed {
                event_index: 0,
                event_word: 0,
                writes: vec![
                    ScriptInitWrite {
                        index: 0,
                        value: VarSetDefault::Dword(12),
                    },
                    ScriptInitWrite {
                        index: 1,
                        value: VarSetDefault::FloatBits(0x3f4c_cccd),
                    },
                ],
            })
        );
    }

    #[test]
    fn script_init_formula_variables_read_prior_writes_and_self() {
        let varset = parse_varset(
            b"VAR(DWORD, source, 0)\nVAR(DWORD, y, 0)\nVAR(DWORD, id, 0)\nVAR(float, target, 0)\nVAR(DWORD, integer, 0)\n",
        )
        .expect("numeric declarations");
        let formulas = FormulaTable::parse(
            b"FUNCTION(literal, 7.0, )\nFUNCTION(add, source + 2.0, )\nFUNCTION(subtract, target - source, )\nFUNCTION(self, target + 1.0, )\nFUNCTION(integer, 10.75 + 0.5, )\nFUNCTION(exponent, target + 1e-2, )\n",
        );
        let package = init_package(vec![
            init_handler19(),
            init_formula_assignment(3, 0),
            init_formula_assignment(3, 1),
            init_formula_assignment(3, 2),
            init_formula_assignment(3, 3),
            init_formula_assignment(4, 4),
            init_formula_assignment(3, 5),
        ]);

        assert_eq!(
            varset.resolve_init(&package, init_input(), Some(&formulas),),
            Ok(ScriptInitResolution::Executed {
                event_index: 0,
                event_word: 0,
                writes: vec![
                    ScriptInitWrite {
                        index: 0,
                        value: VarSetDefault::Dword(10),
                    },
                    ScriptInitWrite {
                        index: 1,
                        value: VarSetDefault::Dword(20),
                    },
                    ScriptInitWrite {
                        index: 2,
                        value: VarSetDefault::Dword(30),
                    },
                    ScriptInitWrite {
                        index: 3,
                        value: VarSetDefault::FloatBits(7.0_f32.to_bits()),
                    },
                    ScriptInitWrite {
                        index: 3,
                        value: VarSetDefault::FloatBits(12.0_f32.to_bits()),
                    },
                    ScriptInitWrite {
                        index: 3,
                        value: VarSetDefault::FloatBits(2.0_f32.to_bits()),
                    },
                    ScriptInitWrite {
                        index: 3,
                        value: VarSetDefault::FloatBits(3.0_f32.to_bits()),
                    },
                    ScriptInitWrite {
                        index: 4,
                        value: VarSetDefault::Dword(11),
                    },
                    ScriptInitWrite {
                        index: 3,
                        value: VarSetDefault::FloatBits(3.01_f32.to_bits()),
                    },
                ],
            })
        );
    }

    #[test]
    fn script_init_formula_atoms_use_native_token_and_name_rules() {
        let varset = parse_varset(
            b"VAR(float, dup, 1)\nVAR(float, dup, 2)\nVAR(float, Name, 3)\nVAR(float, name, 4)\nVAR(float, 1e-2, 41)\nVAR(float, A, 8)\nVAR(float, A1, 9)\nVAR(DWORD, high, 4294967295)\nVAR(float, large, 16777216)\nVAR(float, target, 0)\nVAR(DWORD, integer, 0)\n",
        )
        .expect("numeric declarations");
        let formulas = FormulaTable::parse(
            b"FUNCTION(duplicate, dup + 0, )\nFUNCTION(upper, Name + 0, )\nFUNCTION(lower, name + 0, )\nFUNCTION(numeric_name, 1e-2 + 0, )\nFUNCTION(long_operator_prefix, A1 + 0, )\nFUNCTION(unsigned_dword, high + 0, )\nFUNCTION(negative_exponent, 5e-2 + 2e-2, )\nFUNCTION(positive_exponent, 1e+2 + 2e-2, )\nFUNCTION(binary32_rounding, 16777216 + 1, )\nFUNCTION(cancellation, target - 16777215, )\nFUNCTION(dword_target, 10.75 + 0.5, )\nFUNCTION(single_variable, large, )\nFUNCTION(negative_dword_wrap, 2 - 3, )\nFUNCTION(unsigned_dword_wrap, high + 0, )\n",
        );
        let package = init_package(
            (0..14)
                .map(|formula| {
                    init_formula_assignment(
                        if matches!(formula, 10 | 12 | 13) {
                            10
                        } else {
                            9
                        },
                        formula,
                    )
                })
                .collect(),
        );

        let resolved = varset
            .resolve_init(
                &package,
                Handler19InitInput {
                    first_x87_word: 0,
                    second_x87_word: 0,
                    third_word: 0,
                },
                Some(&formulas),
            )
            .expect("resolve Init")
            .clone();
        assert_eq!(
            resolved,
            ScriptInitResolution::Executed {
                event_index: 0,
                event_word: 0,
                writes: vec![
                    ScriptInitWrite {
                        index: 9,
                        value: VarSetDefault::FloatBits(1.0_f32.to_bits())
                    },
                    ScriptInitWrite {
                        index: 9,
                        value: VarSetDefault::FloatBits(3.0_f32.to_bits())
                    },
                    ScriptInitWrite {
                        index: 9,
                        value: VarSetDefault::FloatBits(4.0_f32.to_bits())
                    },
                    ScriptInitWrite {
                        index: 9,
                        value: VarSetDefault::FloatBits(41.0_f32.to_bits())
                    },
                    ScriptInitWrite {
                        index: 9,
                        value: VarSetDefault::FloatBits(9.0_f32.to_bits())
                    },
                    ScriptInitWrite {
                        index: 9,
                        value: VarSetDefault::FloatBits(4_294_967_296.0_f32.to_bits())
                    },
                    ScriptInitWrite {
                        index: 9,
                        value: VarSetDefault::FloatBits((0.05_f32 + 0.02_f32).to_bits())
                    },
                    ScriptInitWrite {
                        index: 9,
                        value: VarSetDefault::FloatBits((100.0_f32 + 0.02_f32).to_bits())
                    },
                    ScriptInitWrite {
                        index: 9,
                        value: VarSetDefault::FloatBits(16_777_216.0_f32.to_bits())
                    },
                    ScriptInitWrite {
                        index: 9,
                        value: VarSetDefault::FloatBits(1.0_f32.to_bits())
                    },
                    ScriptInitWrite {
                        index: 10,
                        value: VarSetDefault::Dword(11)
                    },
                    ScriptInitWrite {
                        index: 9,
                        value: VarSetDefault::FloatBits(16_777_216.0_f32.to_bits())
                    },
                    ScriptInitWrite {
                        index: 10,
                        value: VarSetDefault::Dword(u32::MAX)
                    },
                    ScriptInitWrite {
                        index: 10,
                        value: VarSetDefault::Dword(0)
                    },
                ],
            }
        );
        assert!(formulas.evaluate_numeric_literal(0).is_err());
    }

    #[test]
    fn script_init_rejects_unsupported_formula_variable_forms() {
        let varset = parse_varset(
            b"VAR(float, target, 1)\nVAR(float, A, 2)\nVAR(float, A1, 3)\nVAR(float, ^name, 4)\n",
        )
        .expect("numeric declarations");
        for expression in [
            &b"target * 2"[..],
            b"-1",
            b"target + 1 + 2",
            b"missing + 1",
            b"A + 1",
            b"^name + 1",
            b"name\xc3\xa9 + 1",
        ] {
            let formulas = one_formula(expression);
            let package = init_package(vec![init_formula_assignment(0, 0)]);
            assert!(
                matches!(
                    varset.resolve_init(
                        &package,
                        Handler19InitInput {
                            first_x87_word: 0,
                            second_x87_word: 0,
                            third_word: 0,
                        },
                        Some(&formulas),
                    ),
                    Ok(ScriptInitResolution::Unsupported {
                        error: ScriptInitResolveError::UnsupportedFormulaExpression {
                            index: 0,
                            ..
                        },
                        ..
                    })
                ),
                "accepted unsupported formula {:?}",
                String::from_utf8_lossy(expression)
            );
        }

        let mut inconsistent = varset;
        inconsistent.declarations[0].default_value = VarSetDefault::Dword(1);
        let formulas = one_formula(b"target + 1");
        let package = init_package(vec![init_formula_assignment(0, 0)]);
        assert!(matches!(
            inconsistent.resolve_init(
                &package,
                Handler19InitInput {
                    first_x87_word: 0,
                    second_x87_word: 0,
                    third_word: 0,
                },
                Some(&formulas),
            ),
            Ok(ScriptInitResolution::Unsupported {
                error: ScriptInitResolveError::UnsupportedFormulaExpression { index: 0, .. },
                ..
            })
        ));

        let mut inconsistent_target =
            parse_varset(b"VAR(float, target, 1)\n").expect("numeric declaration");
        inconsistent_target.declarations[0].default_value = VarSetDefault::Dword(1);
        let formulas = one_formula(b"2.0 + 1");
        assert!(matches!(
            inconsistent_target.resolve_init(
                &init_package(vec![init_formula_assignment(0, 0)]),
                Handler19InitInput {
                    first_x87_word: 0,
                    second_x87_word: 0,
                    third_word: 0,
                },
                Some(&formulas),
            ),
            Ok(ScriptInitResolution::Unsupported {
                error: ScriptInitResolveError::UnsupportedFormulaExpression { index: 0, .. },
                ..
            })
        ));
    }

    #[test]
    fn script_init_keeps_unsupported_formula_expressions_lazy_and_transactional() {
        let varset = parse_varset(b"VAR(DWORD, target, 0)\nVAR(DWORD, integer, 0)\n")
            .expect("numeric declaration");
        let formulas =
            FormulaTable::parse(b"FUNCTION(unsupported, target * 2, )\nFUNCTION(literal, 7.5, )\n");
        let action = |formula_index| ScriptInstruction {
            header_words: [u32::MAX, 1, u32::MAX, u32::MAX, 6, 0, formula_index],
            references: Vec::new(),
        };
        let package = |formula_index| ScriptPackage {
            opcode_handler_count: 73,
            events: vec![ScriptEvent {
                name_len: 4,
                name_raw: b"Init\0".to_vec(),
                event_word: 0,
                instructions: vec![action(formula_index)],
            }],
            trailing_bytes: Vec::new(),
            raw: std::sync::Arc::from(&b""[..]),
        };
        let input = Handler19InitInput {
            first_x87_word: 0,
            second_x87_word: 0,
            third_word: 0,
        };

        assert_eq!(
            varset.resolve_init(&package(1), input, Some(&formulas)),
            Ok(ScriptInitResolution::Executed {
                event_index: 0,
                event_word: 0,
                writes: vec![ScriptInitWrite {
                    index: 1,
                    value: VarSetDefault::Dword(7),
                }],
            })
        );
        assert_eq!(
            varset.resolve_init(&package(0), input, Some(&formulas)),
            Ok(ScriptInitResolution::Unsupported {
                event_index: 0,
                instruction_index: 0,
                error: ScriptInitResolveError::UnsupportedFormulaExpression {
                    index: 0,
                    expression: b" target * 2".to_vec(),
                },
            })
        );

        let mut transactional = package(0);
        transactional.events[0].instructions.insert(0, action(1));
        assert_eq!(
            varset.resolve_init(&transactional, input, Some(&formulas)),
            Ok(ScriptInitResolution::Unsupported {
                event_index: 0,
                instruction_index: 1,
                error: ScriptInitResolveError::UnsupportedFormulaExpression {
                    index: 0,
                    expression: b" target * 2".to_vec(),
                },
            })
        );
    }

    #[test]
    fn script_init_keeps_missing_table_and_bad_indices_typed() {
        let varset = parse_varset(b"VAR(DWORD, integer, 0)\n").expect("numeric declaration");
        let instruction = |target, formula_index| ScriptInstruction {
            header_words: [u32::MAX, target, u32::MAX, u32::MAX, 6, 0, formula_index],
            references: Vec::new(),
        };
        let package = |target, formula_index| ScriptPackage {
            opcode_handler_count: 73,
            events: vec![ScriptEvent {
                name_len: 4,
                name_raw: b"Init\0".to_vec(),
                event_word: 0,
                instructions: vec![instruction(target, formula_index)],
            }],
            trailing_bytes: Vec::new(),
            raw: std::sync::Arc::from(&b""[..]),
        };
        let input = Handler19InitInput {
            first_x87_word: 0,
            second_x87_word: 0,
            third_word: 0,
        };
        let formulas = FormulaTable::parse(b"FUNCTION(literal, 1, )\n");

        assert_eq!(
            varset.resolve_init(&package(0, 0), input, None),
            Ok(ScriptInitResolution::Unsupported {
                event_index: 0,
                instruction_index: 0,
                error: ScriptInitResolveError::MissingFormulaTable,
            })
        );
        assert_eq!(
            varset.resolve_init(&package(1, 0), input, Some(&formulas)),
            Err(ScriptInitResolveError::VarSetIndexOutOfBounds {
                index: 1,
                declarations: 1,
            })
        );
        assert_eq!(
            varset.resolve_init(&package(0, 1), input, Some(&formulas)),
            Err(ScriptInitResolveError::FormulaIndexOutOfBounds { index: 1, rows: 1 })
        );
    }

    #[test]
    fn handler_fifteen_resolves_exact_typed_target_by_place_operands() {
        let varset = parse_varset(
            b"VAR( DWORD, d0, 1)\nVAR( DWORD, d1, 2)\nVAR( DWORD, d2, 3)\nVAR( DWORD, d3, 4)\nVAR( float, f4, 1.5)\nVAR( float, f5, 2.5)\nVAR( float, f6, 3.5)\nVAR( float, f7, 4.5)\nVAR( DWORD, mode, 0x0202)\nVAR( DWORD, place0, 12)\nVAR( DWORD, place1, 13)\n",
        )
        .expect("valid Handler(15) varset");
        let instruction = ScriptInstruction {
            header_words: [15, 0, 0, 0, 0, 11, 0],
            references: (0..11).collect(),
        };

        let resolved = varset
            .resolve_handler15(&instruction)
            .expect("exact Handler(15) layout");
        assert_eq!(
            [
                resolved.word_0,
                resolved.word_1,
                resolved.word_2,
                resolved.word_3
            ],
            [1, 2, 3, 4]
        );
        assert_eq!(
            [
                resolved.scalar_4.to_bits(),
                resolved.scalar_5.to_bits(),
                resolved.scalar_6.to_bits(),
                resolved.scalar_7.to_bits()
            ],
            [
                1.5_f32.to_bits(),
                2.5_f32.to_bits(),
                3.5_f32.to_bits(),
                4.5_f32.to_bits()
            ]
        );
        assert_eq!(
            resolved.target,
            Handler15TargetPayload::ByPlace {
                first: 12,
                second: 13
            }
        );
    }

    #[test]
    fn handler_fifteen_rejects_missing_and_wrongly_typed_operands() {
        let wrong_scalar = parse_varset(
            b"VAR( DWORD, d0, 1)\nVAR( DWORD, d1, 2)\nVAR( DWORD, d2, 3)\nVAR( DWORD, d3, 4)\nVAR( DWORD, wrong, 5)\n",
        )
        .expect("valid varset");
        let instruction = ScriptInstruction {
            header_words: [15, 0, 0, 0, 0, 5, 0],
            references: (0..5).collect(),
        };
        assert_eq!(
            wrong_scalar.resolve_handler15(&instruction),
            Err(Handler15ResolveError::UnexpectedType {
                position: 4,
                expected: VarSetType::Float,
                found: VarSetType::Dword,
            })
        );
        let empty = parse_varset(b"").expect("empty varset");
        assert_eq!(
            empty.resolve_handler15(&ScriptInstruction {
                header_words: [15, 0, 0, 0, 0, 0, 0],
                references: Vec::new(),
            }),
            Err(Handler15ResolveError::MissingReference { position: 0 })
        );
    }

    #[test]
    fn decodes_lossless_event_and_instruction_records() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&73_u32.to_le_bytes());
        bytes.extend_from_slice(&1_u32.to_le_bytes());
        bytes.extend_from_slice(&4_u32.to_le_bytes());
        bytes.extend_from_slice(b"Init\0");
        bytes.extend_from_slice(&9_u32.to_le_bytes());
        bytes.extend_from_slice(&1_u32.to_le_bytes());
        for word in [1_u32, 2, 3, 4, 5, 2] {
            bytes.extend_from_slice(&word.to_le_bytes());
        }
        for reference in [7_u32, 8] {
            bytes.extend_from_slice(&reference.to_le_bytes());
        }
        bytes.extend_from_slice(&6_u32.to_le_bytes());
        bytes.extend_from_slice(&[0xaa, 0xbb]);

        let package = decode(&bytes).expect("valid script package");
        assert_eq!(package.opcode_handler_count, 73);
        assert_eq!(package.events.len(), 1);
        assert_eq!(package.events[0].name_raw, b"Init\0");
        assert_eq!(package.events[0].event_word, 9);
        assert_eq!(
            package.events[0].instructions[0].header_words,
            [1, 2, 3, 4, 5, 2, 6]
        );
        assert_eq!(package.events[0].instructions[0].references, [7, 8]);
        assert_eq!(
            package.events[0].instructions[0].dispatch_selector(),
            ScriptDispatchSelector::Handler(1)
        );
        assert_eq!(package.trailing_bytes, [0xaa, 0xbb]);
    }

    #[test]
    fn rejects_missing_event_nul_and_truncated_references() {
        let mut missing_nul = Vec::new();
        missing_nul.extend_from_slice(&0_u32.to_le_bytes());
        missing_nul.extend_from_slice(&1_u32.to_le_bytes());
        missing_nul.extend_from_slice(&1_u32.to_le_bytes());
        missing_nul.extend_from_slice(b"AB");
        assert_eq!(
            decode(&missing_nul),
            Err(DecodeError::Invalid(
                "script event name is not NUL terminated"
            ))
        );

        let mut truncated = Vec::new();
        truncated.extend_from_slice(&0_u32.to_le_bytes());
        truncated.extend_from_slice(&1_u32.to_le_bytes());
        truncated.extend_from_slice(&0_u32.to_le_bytes());
        truncated.push(0);
        truncated.extend_from_slice(&0_u32.to_le_bytes());
        truncated.extend_from_slice(&1_u32.to_le_bytes());
        for word in [0_u32, 0, 0, 0, 0, 1] {
            truncated.extend_from_slice(&word.to_le_bytes());
        }
        assert!(matches!(
            decode(&truncated),
            Err(DecodeError::UnexpectedEof { .. })
        ));
    }

    #[test]
    fn explicit_limits_bound_event_allocations() {
        let bytes = [0_u8; 8];
        let limits = Limits {
            max_entries: 0,
            ..Limits::default()
        };
        assert!(decode_with_limits(&bytes, limits).is_ok());

        let bytes = [0_u8, 0, 0, 0, 1, 0, 0, 0];
        assert!(matches!(
            decode_with_limits(&bytes, limits),
            Err(DecodeError::LimitExceeded { .. })
        ));
    }

    #[test]
    fn dispatch_selector_preserves_sentinel_and_unobserved_values() {
        let mut instruction = super::ScriptInstruction {
            header_words: [u32::MAX; INSTRUCTION_WORDS],
            references: Vec::new(),
        };
        assert_eq!(
            instruction.dispatch_selector(),
            ScriptDispatchSelector::Sentinel
        );
        instruction.header_words[0] = GOG_HANDLER_COUNT;
        assert_eq!(
            instruction.dispatch_selector(),
            ScriptDispatchSelector::Unknown(GOG_HANDLER_COUNT)
        );
    }
}
