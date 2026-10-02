//! `sql`: the local SQLite database.
//!
//! The server keeps `sv.db`, clients and the menu `cl.db`, in openGMOD's own folder (never in
//! the installation); without one ([`crate::Runtime::set_data_dir`]) the database lives in
//! memory. SQLite is the bundled one, built with the crate.
//!
//! What the engine does here is `sql.Query` and `sql.QueryTyped`; the rest of the library
//! (`sql.SQLStr`, `sql.TableExists`, `sql.LastError`...) is Lua, in the game's
//! `includes/util/sql.lua`. As in GMod:
//! * `sql.Query` runs every statement of the text and returns the rows of all of them, every
//!   value as text and SQL `NULL` as the string `"NULL"`; `nil` when there are none, `false`
//!   on an error, whose message goes to `sql.m_strError` (what `sql.LastError` reads).
//! * `sql.QueryTyped` runs one statement with `?` parameters bound and returns typed values:
//!   numbers, strings, no field for `NULL`; integers beyond 2^53 as strings; `{}` when there
//!   are no rows.

use std::cell::RefCell;

use mlua::{Lua, MultiValue, Table, Value};
use rusqlite::fallible_iterator::FallibleIterator;
use rusqlite::types::{ToSqlOutput, ValueRef};
use rusqlite::{Batch, Connection, Rows, Statement};

use crate::args::Args;
use crate::{Realm, host};

/// The largest integer a Lua number holds exactly.
const MAX_EXACT: i64 = (1 << 53) - 1;

#[derive(Default)]
pub(crate) struct Database(RefCell<Option<Connection>>);

/// The database of the realm, opened on first use.
fn with_connection<R>(lua: &Lua, f: impl FnOnce(&Connection) -> Result<R, String>) -> Result<R, String> {
    let host = host(lua);
    let mut database = host.database.0.borrow_mut();
    if database.is_none() {
        let file = if host.realm == Realm::Server { "sv.db" } else { "cl.db" };
        let path = host.data_dir.borrow().as_ref().and_then(|dir| dir.path(file).ok());
        let connection = match path {
            Some(path) => Connection::open(path),
            None => Connection::open_in_memory(),
        };
        *database = Some(connection.map_err(|e| message(&e))?);
    }
    f(database.as_ref().expect("opened above"))
}

/// SQLite's own message for an error, as `sqlite3_errmsg` gives it.
fn message(error: &rusqlite::Error) -> String {
    match error {
        rusqlite::Error::SqliteFailure(_, Some(text)) | rusqlite::Error::SqlInputError { msg: text, .. } => text.clone(),
        rusqlite::Error::SqliteFailure(failure, None) => failure.to_string(),
        rusqlite::Error::InvalidParameterCount(given, wanted) => format!("{given} parameters given, the query has {wanted}"),
        other => other.to_string(),
    }
}

/// A REAL as SQLite writes it as text: 15 significant digits (17 when 15 do not give the
/// same number back), always with a decimal point.
pub fn real_text(value: f64) -> String {
    if value.is_infinite() {
        return if value > 0.0 { "Inf".into() } else { "-Inf".into() };
    }
    let digits = if format_g(value, 15).parse::<f64>().ok() == Some(value) { 15 } else { 17 };
    format_g(value, digits)
}

/// C's `%!.Ng`: `N` significant digits, trailing zeros dropped but one kept after the point.
fn format_g(value: f64, digits: usize) -> String {
    if value == 0.0 {
        return if value.is_sign_negative() { "-0.0".into() } else { "0.0".into() };
    }
    let scientific = format!("{:.*e}", digits - 1, value);
    let (mantissa, exponent) = scientific.split_once('e').unwrap_or((&scientific, "0"));
    let exponent: i32 = exponent.parse().unwrap_or(0);
    let trim = |text: &str| -> String {
        if !text.contains('.') {
            return format!("{text}.0");
        }
        let trimmed = text.trim_end_matches('0');
        if trimmed.ends_with('.') { format!("{trimmed}0") } else { trimmed.to_string() }
    };
    if exponent < -4 || exponent >= digits as i32 {
        let sign = if exponent < 0 { '-' } else { '+' };
        format!("{}e{sign}{:02}", trim(mantissa), exponent.abs())
    } else {
        let decimals = (digits as i32 - 1 - exponent).max(0) as usize;
        trim(&format!("{value:.decimals$}"))
    }
}

fn set_error(lua: &Lua, error: &str) -> mlua::Result<()> {
    let sql: Table = lua.globals().get("sql")?;
    sql.raw_set("m_strError", error)
}

/// `sql.Query(text)`.
fn query(a: &Args) -> mlua::Result<Value> {
    let text = match a.get(1) {
        Some(Value::String(s)) => s.as_bytes().to_vec(),
        _ => a.string(1)?.into_bytes(),
    };
    let Ok(text) = String::from_utf8(text) else {
        set_error(a.lua, "the query is not UTF-8 text")?;
        return Ok(Value::Boolean(false));
    };
    let lua = a.lua;
    let result = with_connection(lua, |connection| {
        let rows = lua.create_table().map_err(|e| e.to_string())?;
        let mut batch = Batch::new(connection, &text);
        while let Some(mut statement) = batch.next().map_err(|e| message(&e))? {
            let names = column_names(&statement);
            push_rows(lua, &names, statement.query([]).map_err(|e| message(&e))?, &rows, false).map_err(|e| message(&e))?;
        }
        Ok(rows)
    });
    match result {
        Ok(rows) if rows.raw_len() == 0 => Ok(Value::Nil),
        Ok(rows) => Ok(Value::Table(rows)),
        Err(error) => {
            set_error(lua, &error)?;
            Ok(Value::Boolean(false))
        }
    }
}

/// A Lua value bound to a parameter; strings as their bytes, which `text` holds.
fn parameter<'a>(value: &Value, text: &'a [u8]) -> Result<ToSqlOutput<'a>, String> {
    Ok(match value {
        Value::Nil => ToSqlOutput::Borrowed(ValueRef::Null),
        Value::Boolean(b) => ToSqlOutput::Borrowed(ValueRef::Integer(*b as i64)),
        Value::Integer(n) => ToSqlOutput::Borrowed(ValueRef::Integer(*n)),
        Value::Number(n) if n.fract() == 0.0 && n.abs() <= MAX_EXACT as f64 => ToSqlOutput::Borrowed(ValueRef::Integer(*n as i64)),
        Value::Number(n) => ToSqlOutput::Borrowed(ValueRef::Real(*n)),
        Value::String(_) => ToSqlOutput::Borrowed(ValueRef::Text(text)),
        other => return Err(format!("cannot bind a {} to a query parameter", other.type_name())),
    })
}

/// `sql.QueryTyped(text, ...)`.
fn query_typed(a: &Args) -> mlua::Result<Value> {
    let text = a.string(1)?;
    let parameters: Vec<Value> = a.values.iter().skip(1).cloned().collect();
    let texts: Vec<Vec<u8>> = parameters
        .iter()
        .map(|v| match v {
            Value::String(s) => s.as_bytes().to_vec(),
            _ => Vec::new(),
        })
        .collect();
    let lua = a.lua;
    let result = with_connection(lua, |connection| {
        let mut batch = Batch::new(connection, &text);
        let rows = lua.create_table().map_err(|e| e.to_string())?;
        let Some(mut statement) = batch.next().map_err(|e| message(&e))? else { return Ok(rows) };
        if statement.parameter_count() != parameters.len() {
            return Err(format!("{} parameters given, the query has {}", parameters.len(), statement.parameter_count()));
        }
        for (i, (value, text)) in parameters.iter().zip(&texts).enumerate() {
            statement.raw_bind_parameter(i + 1, parameter(value, text)?).map_err(|e| message(&e))?;
        }
        let names = column_names(&statement);
        push_rows(lua, &names, statement.raw_query(), &rows, true).map_err(|e| message(&e))?;
        Ok(rows)
    });
    match result {
        Ok(rows) => Ok(Value::Table(rows)),
        Err(error) => {
            set_error(lua, &error)?;
            Ok(Value::Boolean(false))
        }
    }
}

/// A column value for Lua: as text (`sql.Query`), or typed (`sql.QueryTyped`, `None` for NULL).
fn convert(lua: &Lua, value: ValueRef<'_>, typed: bool) -> mlua::Result<Option<Value>> {
    Ok(Some(match (value, typed) {
        (ValueRef::Null, false) => Value::String(lua.create_string("NULL")?),
        (ValueRef::Null, true) => return Ok(None),
        (ValueRef::Integer(n), false) => Value::String(lua.create_string(n.to_string())?),
        (ValueRef::Integer(n), true) if n.abs() > MAX_EXACT => Value::String(lua.create_string(n.to_string())?),
        (ValueRef::Integer(n), true) => Value::Number(n as f64),
        (ValueRef::Real(r), false) => Value::String(lua.create_string(real_text(r))?),
        (ValueRef::Real(r), true) => Value::Number(r),
        (ValueRef::Text(bytes) | ValueRef::Blob(bytes), _) => Value::String(lua.create_string(bytes)?),
    }))
}

/// Appends the rows of a running statement to `out`.
fn push_rows(lua: &Lua, names: &[String], mut rows: Rows<'_>, out: &Table, typed: bool) -> rusqlite::Result<()> {
    while let Some(row) = rows.next()? {
        let entry = lua.create_table().map_err(lua_error)?;
        for (i, name) in names.iter().enumerate() {
            if let Some(value) = convert(lua, row.get_ref(i)?, typed).map_err(lua_error)? {
                entry.raw_set(name.as_str(), value).map_err(lua_error)?;
            }
        }
        out.raw_push(entry).map_err(lua_error)?;
    }
    Ok(())
}

fn column_names(statement: &Statement<'_>) -> Vec<String> {
    statement.column_names().into_iter().map(str::to_string).collect()
}

fn lua_error(error: mlua::Error) -> rusqlite::Error {
    rusqlite::Error::ToSqlConversionFailure(error.to_string().into())
}

pub(crate) fn install(lua: &Lua) -> mlua::Result<()> {
    let sql = lua.create_table()?;
    for (name, f) in [("Query", query as fn(&Args) -> mlua::Result<Value>), ("QueryTyped", query_typed)] {
        sql.raw_set(name, lua.create_function(move |lua, values: MultiValue| f(&Args::new(lua, values, name)))?)?;
    }
    lua.globals().raw_set("sql", sql)?;
    Ok(())
}
