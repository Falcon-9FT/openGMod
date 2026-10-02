//! The `sql` library: SQLite, one database per realm.
//!
//! GMod keeps `sv.db` (server) and `cl.db` (client and menu) in its base folder; openGMOD
//! keeps them in its own folder ([`crate::Runtime::set_data_dir`]) and never opens the
//! installation's. Without that folder the database lives in memory and is gone with the
//! runtime. It is opened at the first query.
//!
//! The engine gives `sql.Query`, `sql.QueryTyped` and the error in `sql.m_strError`; the rest
//! (`sql.SQLStr`, `sql.TableExists`, `sql.QueryValue`, `sql.Begin` ...) is the game's own Lua
//! (`includes/util/sql.lua`).
//!
//! GMod builds SQLite without `ATTACH`, `DETACH` and `VACUUM`. Here an authorizer refuses
//! them, and the pragmas that name folders: a query from Lua cannot make or open a file
//! anywhere else.

use std::cell::RefCell;

use mlua::{Lua, MultiValue, Table, Value};
use rusqlite::Connection;
use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
use rusqlite::types::ValueRef;

use crate::args::Args;
use crate::{Realm, host};

/// The realm's database, opened when first used.
#[derive(Default)]
pub(crate) struct Database {
    connection: RefCell<Option<Connection>>,
}

impl Database {
    /// Closes the database, so that the next query opens it where the data folder now is.
    pub(crate) fn close(&self) {
        self.connection.borrow_mut().take();
    }
}

/// The file of a realm's database, in openGMOD's folder.
pub fn file_name(realm: Realm) -> &'static str {
    match realm {
        Realm::Server => "sv.db",
        Realm::Client | Realm::Menu => "cl.db",
    }
}

fn authorize(context: AuthContext<'_>) -> Authorization {
    match context.action {
        AuthAction::Attach { .. } | AuthAction::Detach { .. } => Authorization::Deny,
        AuthAction::Pragma { pragma_name, .. }
            if pragma_name.eq_ignore_ascii_case("temp_store_directory") || pragma_name.eq_ignore_ascii_case("data_store_directory") =>
        {
            Authorization::Deny
        }
        _ => Authorization::Allow,
    }
}

fn open(lua: &Lua) -> rusqlite::Result<Connection> {
    let host = host(lua);
    let path = host.data_dir.borrow().as_ref().and_then(|dir| dir.path(file_name(host.realm)).ok());
    let connection = match path {
        Some(path) => Connection::open(path)?,
        None => Connection::open_in_memory()?,
    };
    connection.authorizer(Some(authorize))?;
    Ok(connection)
}

/// Runs `f` on the realm's database, opening it if needed.
fn with<R>(lua: &Lua, f: impl FnOnce(&Connection) -> rusqlite::Result<R>) -> rusqlite::Result<R> {
    let host = host(lua);
    let mut slot = host.database.connection.borrow_mut();
    if slot.is_none() {
        *slot = Some(open(lua)?);
    }
    f(slot.as_ref().expect("opened above"))
}

/// SQLite's own error text, without rusqlite's wrapping.
fn message(error: &rusqlite::Error) -> String {
    match error {
        rusqlite::Error::SqliteFailure(_, Some(text)) | rusqlite::Error::SqlInputError { msg: text, .. } => text.clone(),
        other => other.to_string(),
    }
}

/// A real as SQLite turns it into text: always with a decimal point or an exponent.
fn real_text(n: f64) -> String {
    let text = n.to_string();
    if text.contains(['.', 'e', 'E', 'i', 'N']) { text } else { format!("{text}.0") }
}

/// A row as column names and values.
type Row<T> = Vec<(String, T)>;

/// Every statement of `sql`, in order; the rows of all of them, every value as text.
fn query(connection: &Connection, sql: &str) -> rusqlite::Result<Vec<Row<Vec<u8>>>> {
    use rusqlite::fallible_iterator::FallibleIterator as _;
    let mut rows = Vec::new();
    let mut batch = rusqlite::Batch::new(connection, sql);
    while let Some(mut statement) = batch.next()? {
        let names: Vec<String> = statement.column_names().into_iter().map(str::to_string).collect();
        let mut result = statement.raw_query();
        while let Some(row) = result.next()? {
            let mut out = Vec::with_capacity(names.len());
            for (i, name) in names.iter().enumerate() {
                let text = match row.get_ref(i)? {
                    ValueRef::Null => b"NULL".to_vec(),
                    ValueRef::Integer(n) => n.to_string().into_bytes(),
                    ValueRef::Real(n) => real_text(n).into_bytes(),
                    ValueRef::Text(t) | ValueRef::Blob(t) => t.to_vec(),
                };
                out.push((name.clone(), text));
            }
            rows.push(out);
        }
    }
    Ok(rows)
}

/// `sql.m_strError`, which `sql.LastError` returns.
fn set_error(lua: &Lua, text: &str) -> mlua::Result<()> {
    let sql: Table = lua.globals().get("sql")?;
    sql.raw_set("m_strError", text)
}

/// A Lua value bound to a `?` of `sql.QueryTyped`.
fn parameter(a: &Args, position: usize) -> mlua::Result<rusqlite::types::Value> {
    use rusqlite::types::Value as Sql;
    Ok(match a.values.get(position - 1) {
        None | Some(Value::Nil) => Sql::Null,
        Some(Value::Boolean(b)) => Sql::Integer(i64::from(*b)),
        Some(Value::Integer(n)) => Sql::Integer(*n),
        // a whole number is an INTEGER, as SQLite would store it from Lua's point of view
        Some(Value::Number(n)) if n.fract() == 0.0 && n.abs() < 9.2e18 => Sql::Integer(*n as i64),
        Some(Value::Number(n)) => Sql::Real(*n),
        Some(Value::String(s)) => match std::str::from_utf8(&s.as_bytes()) {
            Ok(text) => Sql::Text(text.to_string()),
            Err(_) => Sql::Blob(s.as_bytes().to_vec()),
        },
        Some(_) => return Err(a.bad(position, "nil, boolean, number or string")),
    })
}

/// The largest integer a double holds exactly; larger ones go back to Lua as strings.
const EXACT: i64 = (1 << 53) - 1;

/// One statement with its `?` bound; the rows, NULL columns left out.
fn query_typed(
    connection: &Connection,
    sql: &str,
    parameters: &[rusqlite::types::Value],
) -> rusqlite::Result<Vec<Row<rusqlite::types::Value>>> {
    let mut statement = connection.prepare(sql)?;
    let names: Vec<String> = statement.column_names().into_iter().map(str::to_string).collect();
    let mut result = statement.query(rusqlite::params_from_iter(parameters))?;
    let mut rows = Vec::new();
    while let Some(row) = result.next()? {
        let mut out = Vec::with_capacity(names.len());
        for (i, name) in names.iter().enumerate() {
            match row.get::<_, rusqlite::types::Value>(i)? {
                rusqlite::types::Value::Null => {}
                value => out.push((name.clone(), value)),
            }
        }
        rows.push(out);
    }
    Ok(rows)
}

pub(crate) fn install(lua: &Lua) -> mlua::Result<()> {
    let sql = lua.create_table()?;
    let def = |name: &'static str, f: fn(&Args) -> mlua::Result<Value>| -> mlua::Result<()> {
        sql.raw_set(name, lua.create_function(move |lua, values: MultiValue| f(&Args::new(lua, values, name)))?)
    };
    def("Query", |a| {
        let text = a.string(1)?;
        match with(a.lua, |connection| query(connection, &text)) {
            Ok(rows) if rows.is_empty() => Ok(Value::Nil),
            Ok(rows) => {
                let out = a.lua.create_table()?;
                for row in rows {
                    let t = a.lua.create_table()?;
                    for (name, value) in row {
                        t.raw_set(name, a.lua.create_string(value)?)?;
                    }
                    out.raw_push(t)?;
                }
                Ok(Value::Table(out))
            }
            Err(e) => {
                set_error(a.lua, &message(&e))?;
                Ok(Value::Boolean(false))
            }
        }
    })?;
    def("QueryTyped", |a| {
        use rusqlite::types::Value as Sql;
        let text = a.string(1)?;
        let parameters: Vec<Sql> = (2..=a.values.len()).map(|i| parameter(a, i)).collect::<mlua::Result<_>>()?;
        match with(a.lua, |connection| query_typed(connection, &text, &parameters)) {
            Ok(rows) => {
                let out = a.lua.create_table()?;
                for row in rows {
                    let t = a.lua.create_table()?;
                    for (name, value) in row {
                        let value = match value {
                            Sql::Integer(n) if (-EXACT..=EXACT).contains(&n) => Value::Number(n as f64),
                            Sql::Integer(n) => Value::String(a.lua.create_string(n.to_string())?),
                            Sql::Real(n) => Value::Number(n),
                            Sql::Text(t) => Value::String(a.lua.create_string(t)?),
                            Sql::Blob(b) => Value::String(a.lua.create_string(b)?),
                            Sql::Null => Value::Nil,
                        };
                        t.raw_set(name, value)?;
                    }
                    out.raw_push(t)?;
                }
                Ok(Value::Table(out))
            }
            Err(e) => {
                set_error(a.lua, &message(&e))?;
                Ok(Value::Boolean(false))
            }
        }
    })?;
    lua.globals().raw_set("sql", sql)
}
