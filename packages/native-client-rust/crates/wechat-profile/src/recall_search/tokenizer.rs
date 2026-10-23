//! Replay existing index terms for one armed deletion. This does not tokenize
//! new text or provide an alternative implementation of the vendor tokenizer.
use crate::{ProfileError, Result};
use rusqlite::{ffi, Connection};
use std::{
    ffi::{c_char, c_int, c_void, CStr},
    ptr,
    sync::{Arc, Mutex},
};

pub(super) struct Token {
    pub bytes: Vec<u8>,
    pub position: i64,
}

struct Replay {
    content: Vec<u8>,
    tokens: Vec<Token>,
    consumed: bool,
}

#[derive(Clone)]
pub(super) struct Gate(Arc<Mutex<Option<Replay>>>);

unsafe extern "C" fn create(
    context: *mut c_void,
    args: *mut *const c_char,
    count: c_int,
    out: *mut *mut ffi::Fts5Tokenizer,
) -> c_int {
    if context.is_null() || out.is_null() || count != 1 || args.is_null() {
        return ffi::SQLITE_ERROR;
    }
    let arg = *args;
    if arg.is_null() || CStr::from_ptr(arg).to_bytes() != b"disable_pinyin" {
        return ffi::SQLITE_ERROR;
    }
    let state = (&*context.cast::<Gate>()).clone();
    *out = Box::into_raw(Box::new(state)).cast();
    ffi::SQLITE_OK
}

unsafe extern "C" fn delete(tokenizer: *mut ffi::Fts5Tokenizer) {
    if !tokenizer.is_null() {
        drop(Box::from_raw(tokenizer.cast::<Gate>()));
    }
}

unsafe extern "C" fn destroy(context: *mut c_void) {
    if !context.is_null() {
        drop(Box::from_raw(context.cast::<Gate>()));
    }
}

type Emit = unsafe extern "C" fn(*mut c_void, c_int, *const c_char, c_int, c_int, c_int) -> c_int;

unsafe extern "C" fn tokenize(
    tokenizer: *mut ffi::Fts5Tokenizer,
    context: *mut c_void,
    flags: c_int,
    text: *const c_char,
    length: c_int,
    emit: Option<Emit>,
) -> c_int {
    // FTS5 uses DOCUMENT for both insertion and deletion. The private caller
    // arms this callback only around its exact DELETE, never a general write.
    if tokenizer.is_null() || flags != 4 || length < 0 || (text.is_null() && length != 0) {
        return ffi::SQLITE_ERROR;
    }
    let Some(emit) = emit else {
        return ffi::SQLITE_ERROR;
    };
    let state = &*tokenizer.cast::<Gate>();
    let Ok(mut state) = state.0.try_lock() else {
        return ffi::SQLITE_ERROR;
    };
    let Some(replay) = state.as_mut() else {
        return ffi::SQLITE_ERROR;
    };
    let bytes = if length == 0 {
        &[]
    } else {
        std::slice::from_raw_parts(text.cast::<u8>(), length as usize)
    };
    if replay.consumed || bytes != replay.content {
        return ffi::SQLITE_ERROR;
    }
    replay.consumed = true;
    let mut previous = None;
    for token in &replay.tokens {
        let colocated = i32::from(previous == Some(token.position));
        let result = emit(
            context,
            colocated,
            token.bytes.as_ptr().cast(),
            token.bytes.len() as c_int,
            0,
            length,
        );
        if result != ffi::SQLITE_OK {
            return result;
        }
        previous = Some(token.position);
    }
    ffi::SQLITE_OK
}

/// The connection retains the registration and owns context until it closes.
/// Tokenizer instances hold independent references to the same private gate.
pub(super) fn register(connection: &Connection) -> Result<Gate> {
    let gate = Gate(Arc::new(Mutex::new(None)));
    let context = Box::into_raw(Box::new(gate.clone())).cast();
    let tokenizer = ffi::fts5_tokenizer {
        xCreate: Some(create),
        xDelete: Some(delete),
        xTokenize: Some(tokenize),
    };
    if let Err(error) = unsafe { install(connection, tokenizer, context, Some(destroy)) } {
        unsafe { destroy(context) };
        return Err(error);
    }
    Ok(gate)
}

/// Register callbacks owned by this private connection.
///
/// # Safety
/// `context` must remain valid for every callback until `destructor` runs, and
/// all three tokenizer callbacks must obey SQLite's C ABI and pointer lifetimes.
/// On failure the caller retains ownership of `context`.
pub(super) unsafe fn install(
    connection: &Connection,
    mut tokenizer: ffi::fts5_tokenizer,
    context: *mut c_void,
    destructor: Option<unsafe extern "C" fn(*mut c_void)>,
) -> Result<()> {
    let mut statement = ptr::null_mut();
    let mut api: *mut ffi::fts5_api = ptr::null_mut();
    let prepared = ffi::sqlite3_prepare_v2(
        connection.handle(),
        c"SELECT fts5(?1)".as_ptr(),
        -1,
        &mut statement,
        ptr::null_mut(),
    );
    if prepared != ffi::SQLITE_OK {
        return Err(ProfileError::Database);
    }
    let bound = ffi::sqlite3_bind_pointer(
        statement,
        1,
        (&mut api as *mut *mut ffi::fts5_api).cast(),
        c"fts5_api_ptr".as_ptr(),
        None,
    );
    let stepped = if bound == ffi::SQLITE_OK {
        ffi::sqlite3_step(statement)
    } else {
        bound
    };
    let finalized = ffi::sqlite3_finalize(statement);
    if stepped != ffi::SQLITE_ROW || finalized != ffi::SQLITE_OK || api.is_null() {
        return Err(ProfileError::Database);
    }
    let Some(register) = (*api).xCreateTokenizer else {
        return Err(ProfileError::Database);
    };
    if register(
        api,
        c"MMFtsTokenizer".as_ptr(),
        context,
        &mut tokenizer,
        destructor,
    ) != ffi::SQLITE_OK
    {
        return Err(ProfileError::Database);
    }
    Ok(())
}

impl Gate {
    pub(super) fn arm(&self, content: Vec<u8>, tokens: Vec<Token>) -> Result<()> {
        let mut state = self.0.lock().map_err(|_| ProfileError::Database)?;
        if state.is_some() {
            return Err(ProfileError::InvalidMetadata);
        }
        *state = Some(Replay {
            content,
            tokens,
            consumed: false,
        });
        Ok(())
    }

    pub(super) fn disarm(&self) -> Result<()> {
        let replay = self
            .0
            .lock()
            .map_err(|_| ProfileError::Database)?
            .take()
            .ok_or(ProfileError::InvalidMetadata)?;
        if !replay.consumed && (!replay.content.is_empty() || !replay.tokens.is_empty()) {
            return Err(ProfileError::InvalidMetadata);
        }
        Ok(())
    }
}
