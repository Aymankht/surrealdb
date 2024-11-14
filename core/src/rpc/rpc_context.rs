use std::{collections::BTreeMap, mem};

#[cfg(all(not(target_arch = "wasm32"), surrealdb_unstable))]
use async_graphql::BatchRequest;
use uuid::Uuid;

#[cfg(all(not(target_arch = "wasm32"), surrealdb_unstable))]
use crate::gql::SchemaCache;
use crate::{
	dbs::{capabilities::MethodTarget, QueryType, Response, Session},
	kvs::Datastore,
	rpc::args::Take,
	sql::{
		statements::{
			CreateStatement, DeleteStatement, InsertStatement, KillStatement, LiveStatement,
			RelateStatement, SelectStatement, UpdateStatement, UpsertStatement,
		},
		Array, Fields, Function, Model, Output, Query, Strand, Value,
	},
};

use super::{method::Method, response::Data, rpc_error::RpcError, statement_options::StatementOptions};

#[allow(async_fn_in_trait)]
pub trait RpcContext {
	fn kvs(&self) -> &Datastore;
	fn session(&self) -> &Session;
	fn session_mut(&mut self) -> &mut Session;
	fn vars(&self) -> &BTreeMap<String, Value>;
	fn vars_mut(&mut self) -> &mut BTreeMap<String, Value>;
	fn version_data(&self) -> Data;

	const LQ_SUPPORT: bool = false;
	fn handle_live(&self, _lqid: &Uuid) -> impl std::future::Future<Output = ()> + Send {
		async { unimplemented!("handle functions must be redefined if LQ_SUPPORT = true") }
	}
	fn handle_kill(&self, _lqid: &Uuid) -> impl std::future::Future<Output = ()> + Send {
		async { unimplemented!("handle functions must be redefined if LQ_SUPPORT = true") }
	}

	#[cfg(all(not(target_arch = "wasm32"), surrealdb_unstable))]
	const GQL_SUPPORT: bool = false;

	#[cfg(all(not(target_arch = "wasm32"), surrealdb_unstable))]
	fn graphql_schema_cache(&self) -> &SchemaCache {
		unimplemented!("graphql_schema_cache must be implemented if GQL_SUPPORT = true")
	}

	/// Executes any method on this RPC implementation
	async fn execute(&mut self, method: Method, params: Array) -> Result<Data, RpcError> {
		// Check if capabilities allow executing the requested RPC method
		if !self.kvs().allows_rpc_method(&MethodTarget {
			method,
		}) {
			warn!("Capabilities denied RPC method call attempt, target: '{}'", method.to_str());
			return Err(RpcError::MethodNotAllowed);
		}
		// Execute the desired method
		match method {
			Method::Ping => Ok(Value::None.into()),
			Method::Info => self.info().await,
			Method::Use => self.yuse(params).await,
			Method::Signup => self.signup(params).await,
			Method::Signin => self.signin(params).await,
			Method::Invalidate => self.invalidate().await,
			Method::Authenticate => self.authenticate(params).await,
			Method::Kill => self.kill(params).await,
			Method::Live => self.live(params).await,
			Method::Set => self.set(params).await,
			Method::Unset => self.unset(params).await,
			Method::Select => self.select(params).await,
			Method::Insert => self.insert(params).await,
			Method::Create => self.create(params).await,
			Method::Upsert => self.upsert(params).await,
			Method::Update => self.update(params).await,
			Method::Merge => self.merge(params).await,
			Method::Patch => self.patch(params).await,
			Method::Delete => self.delete(params).await,
			Method::Version => self.version(params).await,
			Method::Query => self.query(params).await,
			Method::Relate => self.relate(params).await,
			Method::Run => self.run(params).await,
			Method::GraphQL => self.graphql(params).await,
			Method::InsertRelation => self.insert_relation(params).await,
			Method::Unknown => Err(RpcError::MethodNotFound),
		}
	}

	/// Executes any immutable method on this RPC implementation
	async fn execute_immut(&self, method: Method, params: Array) -> Result<Data, RpcError> {
		// Check if capabilities allow executing the requested RPC method
		if !self.kvs().allows_rpc_method(&MethodTarget {
			method,
		}) {
			warn!("Capabilities denied RPC method call attempt, target: '{}'", method.to_str());
			return Err(RpcError::MethodNotAllowed);
		}
		// Execute the desired method
		match method {
			Method::Ping => Ok(Value::None.into()),
			Method::Info => self.info().await,
			Method::Select => self.select(params).await,
			Method::Insert => self.insert(params).await,
			Method::Create => self.create(params).await,
			Method::Upsert => self.upsert(params).await,
			Method::Update => self.update(params).await,
			Method::Merge => self.merge(params).await,
			Method::Patch => self.patch(params).await,
			Method::Delete => self.delete(params).await,
			Method::Version => self.version(params).await,
			Method::Query => self.query(params).await,
			Method::Relate => self.relate(params).await,
			Method::Run => self.run(params).await,
			Method::GraphQL => self.graphql(params).await,
			Method::InsertRelation => self.insert_relation(params).await,
			Method::Unknown => Err(RpcError::MethodNotFound),
			_ => Err(RpcError::MethodNotFound),
		}
	}

	// ------------------------------
	// Methods for authentication
	// ------------------------------

	async fn yuse(&mut self, params: Array) -> Result<Data, RpcError> {
		// For both ns+db, string = change, null = unset, none = do nothing
		// We need to be able to adjust either ns or db without affecting the other
		// To be able to select a namespace, and then list resources in that namespace, as an example
		let (ns, db) = params.needs_two()?;
		// Update the selected namespace
		match ns {
			Value::None => (),
			Value::Null => self.session_mut().ns = None,
			Value::Strand(ns) => self.session_mut().ns = Some(ns.0),
			_ => {
				return Err(RpcError::InvalidParams);
			}
		}
		// Update the selected database
		match db {
			Value::None => (),
			Value::Null => self.session_mut().db = None,
			Value::Strand(db) => self.session_mut().db = Some(db.0),
			_ => {
				return Err(RpcError::InvalidParams);
			}
		}
		// Clear any residual database
		if self.session().ns.is_none() && self.session().db.is_some() {
			self.session_mut().db = None;
		}
		// Return nothing
		Ok(Value::None.into())
	}

	async fn signup(&mut self, params: Array) -> Result<Data, RpcError> {
		// Process the method arguments
		let Ok(Value::Object(v)) = params.needs_one() else {
			return Err(RpcError::InvalidParams);
		};
		let mut tmp_session = mem::take(self.session_mut());

		let out: Result<Value, RpcError> =
			crate::iam::signup::signup(self.kvs(), &mut tmp_session, v)
				.await
				.map(Into::into)
				.map_err(Into::into);

		*self.session_mut() = tmp_session;
		out.map(Into::into)
	}

	async fn signin(&mut self, params: Array) -> Result<Data, RpcError> {
		// Process the method arguments
		let Ok(Value::Object(v)) = params.needs_one() else {
			return Err(RpcError::InvalidParams);
		};
		let mut tmp_session = mem::take(self.session_mut());
		let out: Result<Value, RpcError> =
			crate::iam::signin::signin(self.kvs(), &mut tmp_session, v)
				.await
				.map(Into::into)
				.map_err(Into::into);
		*self.session_mut() = tmp_session;
		out.map(Into::into)
	}

	async fn invalidate(&mut self) -> Result<Data, RpcError> {
		crate::iam::clear::clear(self.session_mut())?;
		Ok(Value::None.into())
	}

	async fn authenticate(&mut self, params: Array) -> Result<Data, RpcError> {
		// Process the method arguments
		let Ok(Value::Strand(token)) = params.needs_one() else {
			return Err(RpcError::InvalidParams);
		};
		let mut tmp_session = mem::take(self.session_mut());
		let out: Result<(), RpcError> =
			crate::iam::verify::token(self.kvs(), &mut tmp_session, &token.0)
				.await
				.map_err(Into::into);
		*self.session_mut() = tmp_session;
		out.map(|_| Value::None.into())
	}

	// ------------------------------
	// Methods for identification
	// ------------------------------

	async fn info(&self) -> Result<Data, RpcError> {
		// Specify the SQL query string
		let sql = {
			// SELECT * FROM $auth
			SelectStatement {
				expr: Fields::all(),
				what: vec![Value::Param("auth".into())].into(),
				..Default::default()
			}
			.into()
		};
		// Execute the query on the database
		let mut res = self.kvs().process(sql, self.session(), None).await?;
		// Extract the first value from the result
		Ok(res.remove(0).result?.first().into())
	}

	// ------------------------------
	// Methods for setting variables
	// ------------------------------

	async fn set(&mut self, params: Array) -> Result<Data, RpcError> {
		// Process the method arguments
		let Ok((Value::Strand(key), val)) = params.needs_one_or_two() else {
			return Err(RpcError::InvalidParams);
		};
		// Specify the query parameters
		let var = Some(map! {
			key.0.clone() => Value::None,
			=> &self.vars()
		});
		// Compute the specified parameter
		match self.kvs().compute(val, self.session(), var).await? {
			// Remove the variable if undefined
			Value::None => self.vars_mut().remove(&key.0),
			// Store the variable if defined
			v => self.vars_mut().insert(key.0, v),
		};
		// Return nothing
		Ok(Value::Null.into())
	}

	async fn unset(&mut self, params: Array) -> Result<Data, RpcError> {
		// Process the method arguments
		let Ok(Value::Strand(key)) = params.needs_one() else {
			return Err(RpcError::InvalidParams);
		};
		// Remove the set parameter
		self.vars_mut().remove(&key.0);
		// Return nothing
		Ok(Value::Null.into())
	}

	// ------------------------------
	// Methods for live queries
	// ------------------------------

	async fn kill(&mut self, params: Array) -> Result<Data, RpcError> {
		// Process the method arguments
		let id = params.needs_one()?;
		// Specify the SQL query string
		let sql = {
			// KILL $id
			KillStatement {
				id: Value::Param("id".into()),
			}
			.into()
		};
		// Specify the query parameters
		let var = map! {
			String::from("id") => id,
			=> &self.vars()
		};
		// Execute the query on the database
		let mut res = self.query_inner(Value::Query(sql), Some(var)).await?;
		// Extract the first query result
		Ok(res.remove(0).result?.into())
	}

	async fn live(&mut self, params: Array) -> Result<Data, RpcError> {
		// Process the method arguments
		let (what, diff) = params.needs_one_or_two()?;
		// Specify the SQL query string
		let sql = if diff.is_true() {
			// LIVE SELECT DIFF FROM $what
			LiveStatement {
				expr: Fields::default(),
				what: vec![Value::Param("what".into())].into(),
				..Default::default()
			}
			.into()
		} else {
			// LIVE SELECT * FROM $what
			LiveStatement {
				expr: Fields::all(),
				what: vec![Value::Param("what".into())].into(),
				..Default::default()
			}
			.into()
		};
		// Specify the query parameters
		let var = map! {
			String::from("what") => what.could_be_table(),
			=> &self.vars()
		};
		// Execute the query on the database
		let mut res = self.query_inner(Value::Query(sql), Some(var)).await?;
		// Extract the first query result
		Ok(res.remove(0).result?.into())
	}

	// ------------------------------
	// Methods for selecting
	// ------------------------------

	async fn select(&self, params: Array) -> Result<Data, RpcError> {
		// Process the method arguments
		let Ok(what) = params.needs_one() else {
			return Err(RpcError::InvalidParams);
		};
		// Return a single result?
		let one = what.is_thing_single();
		// Specify the SQL query string
		let sql = {
			// SELECT * FROM $what
			SelectStatement {
				expr: Fields::all(),
				what: vec![Value::Param("what".into())].into(),
				..Default::default()
			}
			.into()
		};
		// Specify the query parameters
		let var = Some(map! {
			String::from("what") => what.could_be_table(),
			=> &self.vars()
		});
		// Execute the query on the database
		let mut res = self.kvs().process(sql, self.session(), var).await?;
		// Extract the first query result
		Ok(match one {
			true => res.remove(0).result?.first().into(),
			false => res.remove(0).result?.into(),
		})
	}

	// ------------------------------
	// Methods for inserting
	// ------------------------------

	async fn insert(&self, params: Array) -> Result<Data, RpcError> {
		// Process the method arguments
		let Ok((what, data)) = params.needs_two() else {
			return Err(RpcError::InvalidParams);
		};
		// Process the insert request
		let mut res = match what {
			Value::None | Value::Null => {
				// Specify the SQL query string
				let sql = {
					// INSERT $data RETURN AFTER
					InsertStatement {
						data: crate::sql::Data::SingleExpression(Value::Param("data".into())),
						output: Some(Output::After),
						..Default::default()
					}
					.into()
				};
				// Specify the query parameters
				let var = Some(map! {
					String::from("data") => data,
					=> &self.vars()
				});
				// Execute the query on the database
				self.kvs().process(sql, self.session(), var).await?
			}
			what => {
				// Specify the SQL query string
				let sql = {
					// INSERT INTO $what $data RETURN AFTER
					InsertStatement {
						into: Some(Value::Param("what".into())),
						data: crate::sql::Data::SingleExpression(Value::Param("data".into())),
						output: Some(Output::After),
						..Default::default()
					}
					.into()
				};
				// Specify the query parameters
				let var = Some(map! {
					String::from("what") => what.could_be_table(),
					String::from("data") => data,
					=> &self.vars()
				});
				// Execute the query on the database
				self.kvs().process(sql, self.session(), var).await?
			}
		};
		// Extract the first query result
		Ok(res.remove(0).result?.into())
	}

	async fn insert_relation(&self, params: Array) -> Result<Data, RpcError> {
		// Process the method arguments
		let Ok((what, data)) = params.needs_two() else {
			return Err(RpcError::InvalidParams);
		};
		// Process the insert request
		let mut res = match what {
			Value::None | Value::Null => {
				// Specify the SQL query string
				let sql = {
					// INSERT RELATION $data RETURN AFTER
					InsertStatement {
						relation: true,
						data: crate::sql::Data::SingleExpression(Value::Param("data".into())),
						output: Some(Output::After),
						..Default::default()
					}
					.into()
				};
				// Specify the query parameters
				let vars = Some(map! {
					String::from("data") => data,
					=> &self.vars()
				});
				// Execute the query on the database
				self.kvs().process(sql, self.session(), vars).await?
			}
			Value::Table(_) | Value::Strand(_) => {
				// Specify the SQL query string
				let sql = {
					// INSERT RELATION INTO $what $data RETURN AFTER
					InsertStatement {
						relation: true,
						into: Some(Value::Param("what".into())),
						data: crate::sql::Data::SingleExpression(Value::Param("data".into())),
						output: Some(Output::After),
						..Default::default()
					}
					.into()
				};
				// Specify the query parameters
				let vars = Some(map! {
					String::from("data") => data,
					String::from("what") => what.could_be_table(),
					=> &self.vars()
				});
				// Execute the query on the database
				self.kvs().process(sql, self.session(), vars).await?
			}
			_ => return Err(RpcError::InvalidParams),
		};
		// Extract the first query result
		Ok(res.remove(0).result?.into())
	}

	// ------------------------------
	// Methods for creating
	// ------------------------------

	async fn create(&self, params: Array) -> Result<Data, RpcError> {
		// Process the method arguments
		let Ok((what, data)) = params.needs_one_or_two() else {
			return Err(RpcError::InvalidParams);
		};
		let what = what.could_be_table();
		// Return a single result?
		let one = what.is_thing_single() || what.is_table();
		// Specify the SQL query string
		let sql = if data.is_none_or_null() {
			// CREATE $what RETURN AFTER
			CreateStatement {
				what: vec![Value::Param("what".into())].into(),
				output: Some(Output::After),
				..Default::default()
			}
			.into()
		} else {
			// CREATE $what CONTENT $data RETURN AFTER
			CreateStatement {
				what: vec![Value::Param("what".into())].into(),
				data: Some(crate::sql::Data::MergeExpression(Value::Param("data".into()))),
				output: Some(Output::After),
				..Default::default()
			}
			.into()
		};
		// Specify the query parameters
		let var = Some(map! {
			String::from("what") => what,
			String::from("data") => data,
			=> &self.vars()
		});
		// Execute the query on the database
		let mut res = self.kvs().process(sql, self.session(), var).await?;
		// Extract the first query result
		Ok(match one {
			true => res.remove(0).result?.first().into(),
			false => res.remove(0).result?.into(),
		})
	}

	// ------------------------------
	// Methods for upserting
	// ------------------------------

	async fn upsert(&self, params: Array) -> Result<Data, RpcError> {
		// Process the method arguments
		let Ok((what, data, opts_value)) = params.needs_one_two_or_three() else {
			return Err(RpcError::InvalidParams);
		};
		// Prepare options
		let mut opts = StatementOptions::default();
		// Insert data
		if !data.is_none_or_null() {
			opts.with_data_content(data);
		}
		// Apply user options
		if !opts_value.is_none_or_null() {
			opts.process_options(opts_value)?;
		}
		// Return a single result?
		let one = what.is_thing_single();
		// Get the variables
		let vars = Some(opts.merge_vars(self.vars()));
		// Prepare the SQL statement
		let sql = UpsertStatement {
			what: vec![what].into(),
			data: opts.data_expr(),
			output: Some(opts.output),
			cond: opts.cond,
			..Default::default()
		}
		.into();
		// Execute the statement on the database
		let mut res = self.kvs().process(sql, self.session(), vars).await?;
		// Extract the first statement result
		Ok(match one {
			true => res.remove(0).result?.first().into(),
			false => res.remove(0).result?.into(),
		})
	}

	// ------------------------------
	// Methods for updating
	// ------------------------------

	async fn update(&self, params: Array) -> Result<Data, RpcError> {
		// Process the method arguments
		let Ok((what, data, opts_value)) = params.needs_one_two_or_three() else {
			return Err(RpcError::InvalidParams);
		};
		// Prepare options
		let mut opts = StatementOptions::default();
		// Insert data
		if !data.is_none_or_null() {
			opts.with_data_content(data);
		}
		// Apply user options
		if !opts_value.is_none_or_null() {
			opts.process_options(opts_value)?;
		}
		// Return a single result?
		let one = what.is_thing_single();
		// Get the variables
		let vars = Some(opts.merge_vars(self.vars()));
		// Prepare the SQL statement
		let sql = UpdateStatement {
			what: vec![what].into(),
			data: opts.data_expr(),
			output: Some(opts.output),
			cond: opts.cond,
			..Default::default()
		}
		.into();
		// Execute the statement on the database
		let mut res = self.kvs().process(sql, self.session(), vars).await?;
		// Extract the first statement result
		Ok(match one {
			true => res.remove(0).result?.first().into(),
			false => res.remove(0).result?.into(),
		})
	}

	// ------------------------------
	// Methods for merging
	// ------------------------------

	async fn merge(&self, params: Array) -> Result<Data, RpcError> {
		// Process the method arguments
		let Ok((what, data)) = params.needs_one_or_two() else {
			return Err(RpcError::InvalidParams);
		};
		// Return a single result?
		let one = what.is_thing_single();
		// Specify the SQL query string
		let sql = if data.is_none_or_null() {
			// UPDATE $what RETURN AFTER
			UpdateStatement {
				what: vec![Value::Param("what".into())].into(),
				output: Some(Output::After),
				..Default::default()
			}
			.into()
		} else {
			// UPDATE $what MERGE $data RETURN AFTER
			UpdateStatement {
				what: vec![Value::Param("what".into())].into(),
				data: Some(crate::sql::Data::MergeExpression(Value::Param("data".into()))),
				output: Some(Output::After),
				..Default::default()
			}
			.into()
		};
		// Specify the query parameters
		let var = Some(map! {
			String::from("what") => what.could_be_table(),
			String::from("data") => data,
			=> &self.vars()
		});
		// Execute the query on the database
		let mut res = self.kvs().process(sql, self.session(), var).await?;
		// Extract the first query result
		Ok(match one {
			true => res.remove(0).result?.first().into(),
			false => res.remove(0).result?.into(),
		})
	}

	// ------------------------------
	// Methods for patching
	// ------------------------------

	async fn patch(&self, params: Array) -> Result<Data, RpcError> {
		// Process the method arguments
		let Ok((what, data, diff)) = params.needs_one_two_or_three() else {
			return Err(RpcError::InvalidParams);
		};
		// Return a single result?
		let one = what.is_thing_single();
		// Specify the SQL query string
		let sql = if diff.is_true() {
			// UPDATE $what PATCH $data RETURN DIFF
			UpdateStatement {
				what: vec![Value::Param("what".into())].into(),
				data: Some(crate::sql::Data::PatchExpression(Value::Param("data".into()))),
				output: Some(Output::Diff),
				..Default::default()
			}
			.into()
		} else {
			// UPDATE $what PATCH $data RETURN AFTER
			UpdateStatement {
				what: vec![Value::Param("what".into())].into(),
				data: Some(crate::sql::Data::PatchExpression(Value::Param("data".into()))),
				output: Some(Output::After),
				..Default::default()
			}
			.into()
		};
		// Specify the query parameters
		let var = Some(map! {
			String::from("what") => what.could_be_table(),
			String::from("data") => data,
			=> &self.vars()
		});
		// Execute the query on the database
		let mut res = self.kvs().process(sql, self.session(), var).await?;
		// Extract the first query result
		Ok(match one {
			true => res.remove(0).result?.first().into(),
			false => res.remove(0).result?.into(),
		})
	}

	// ------------------------------
	// Methods for relating
	// ------------------------------

	async fn relate(&self, params: Array) -> Result<Data, RpcError> {
		// Process the method arguments
		let Ok((from, kind, to, data)) = params.needs_three_or_four() else {
			return Err(RpcError::InvalidParams);
		};
		// Return a single result?
		let one = from.is_single() && to.is_single();
		// Specify the SQL query string
		let sql = if data.is_none_or_null() {
			// RELATE $from->$kind->$to RETURN AFTER
			RelateStatement {
				from: Value::Param("from".into()),
				kind: Value::Param("kind".into()),
				with: Value::Param("to".into()),
				output: Some(Output::After),
				..Default::default()
			}
			.into()
		} else {
			// RELATE $from->$kind->$to CONTENT $data RETURN AFTER
			RelateStatement {
				from: Value::Param("from".into()),
				kind: Value::Param("kind".into()),
				with: Value::Param("to".into()),
				data: Some(crate::sql::Data::ContentExpression(Value::Param("data".into()))),
				output: Some(Output::After),
				..Default::default()
			}
			.into()
		};
		// Specify the query parameters
		let var = Some(map! {
			String::from("from") => from,
			String::from("kind") => kind.could_be_table(),
			String::from("to") => to,
			String::from("data") => data,
			=> &self.vars()
		});
		// Execute the query on the database
		let mut res = self.kvs().process(sql, self.session(), var).await?;
		// Extract the first query result
		Ok(match one {
			true => res.remove(0).result?.first().into(),
			false => res.remove(0).result?.into(),
		})
	}

	// ------------------------------
	// Methods for deleting
	// ------------------------------

	async fn delete(&self, params: Array) -> Result<Data, RpcError> {
		// Process the method arguments
		let Ok(what) = params.needs_one() else {
			return Err(RpcError::InvalidParams);
		};
		// Return a single result?
		let one = what.is_thing_single();
		// Specify the SQL query string
		let sql = {
			// DELETE $what RETURN BEFORE
			DeleteStatement {
				what: vec![Value::Param("what".into())].into(),
				output: Some(Output::Before),
				..Default::default()
			}
			.into()
		};
		// Specify the query parameters
		let var = Some(map! {
			String::from("what") => what.could_be_table(),
			=> &self.vars()
		});
		// Execute the query on the database
		let mut res = self.kvs().process(sql, self.session(), var).await?;
		// Extract the first query result
		Ok(match one {
			true => res.remove(0).result?.first().into(),
			false => res.remove(0).result?.into(),
		})
	}

	// ------------------------------
	// Methods for getting info
	// ------------------------------

	async fn version(&self, params: Array) -> Result<Data, RpcError> {
		match params.len() {
			0 => Ok(self.version_data()),
			_ => Err(RpcError::InvalidParams),
		}
	}

	// ------------------------------
	// Methods for querying
	// ------------------------------

	async fn query(&self, params: Array) -> Result<Data, RpcError> {
		// Process the method arguments
		let Ok((query, o)) = params.needs_one_or_two() else {
			return Err(RpcError::InvalidParams);
		};
		if !(query.is_query() || query.is_strand()) {
			return Err(RpcError::InvalidParams);
		}

		let o = match o {
			Value::Object(v) => Some(v),
			Value::None | Value::Null => None,
			_ => return Err(RpcError::InvalidParams),
		};

		// Specify the query parameters
		let vars = match o {
			Some(mut v) => Some(mrg! {v.0, &self.vars()}),
			None => Some(self.vars().clone()),
		};
		self.query_inner(query, vars).await.map(Into::into)
	}

	// ------------------------------
	// Methods for running functions
	// ------------------------------

	async fn run(&self, params: Array) -> Result<Data, RpcError> {
		// Process the method arguments
		let Ok((name, version, args)) = params.needs_one_two_or_three() else {
			return Err(RpcError::InvalidParams);
		};
		// Parse the function name argument
		let name = match name {
			Value::Strand(Strand(v)) => v,
			_ => return Err(RpcError::InvalidParams),
		};
		// Parse any function version argument
		let version = match version {
			Value::Strand(Strand(v)) => Some(v),
			Value::None | Value::Null => None,
			_ => return Err(RpcError::InvalidParams),
		};
		// Parse the function arguments if specified
		let args = match args {
			Value::Array(Array(arr)) => arr,
			Value::None | Value::Null => vec![],
			_ => return Err(RpcError::InvalidParams),
		};
		// Specify the function to run
		let func: Query = match &name[0..4] {
			"fn::" => Function::Custom(name.chars().skip(4).collect(), args).into(),
			"ml::" => Model {
				name: name.chars().skip(4).collect(),
				version: version.ok_or(RpcError::InvalidParams)?,
				args,
			}
			.into(),
			_ => Function::Normal(name, args).into(),
		};
		//
		// Specify the query variables
		let vars = Some(self.vars().clone());
		// Execute the function on the database
		let mut res = self.kvs().process(func, self.session(), vars).await?;
		// Extract the first query result
		Ok(res.remove(0).result?.into())
	}

	// ------------------------------
	// Methods for querying with GraphQL
	// ------------------------------

	#[cfg(any(target_arch = "wasm32", not(surrealdb_unstable)))]
	async fn graphql(&self, _params: Array) -> Result<Data, RpcError> {
		Err(RpcError::MethodNotFound)
	}

	#[cfg(all(not(target_arch = "wasm32"), surrealdb_unstable))]
	async fn graphql(&self, params: Array) -> Result<impl Into<Data>, RpcError> {
		if !*GRAPHQL_ENABLE {
			return Err(RpcError::BadGQLConfig);
		}

		use serde::Serialize;

		use crate::{cnf::GRAPHQL_ENABLE, gql};

		if !Self::GQL_SUPPORT {
			return Err(RpcError::BadGQLConfig);
		}

		let Ok((query, options)) = params.needs_one_or_two() else {
			return Err(RpcError::InvalidParams);
		};

		enum GraphQLFormat {
			Json,
			Cbor,
		}

		let mut pretty = false;
		let mut format = GraphQLFormat::Json;
		match options {
			Value::Object(o) => {
				for (k, v) in o {
					match (k.as_str(), v) {
						("pretty", Value::Bool(b)) => pretty = b,
						("format", Value::Strand(s)) => match s.as_str() {
							"json" => format = GraphQLFormat::Json,
							"cbor" => format = GraphQLFormat::Cbor,
							_ => return Err(RpcError::InvalidParams),
						},
						_ => return Err(RpcError::InvalidParams),
					}
				}
			}
			_ => return Err(RpcError::InvalidParams),
		}

		let req = match query {
			Value::Strand(s) => match format {
				GraphQLFormat::Json => {
					let tmp: BatchRequest =
						serde_json::from_str(s.as_str()).map_err(|_| RpcError::ParseError)?;
					tmp.into_single().map_err(|_| RpcError::ParseError)?
				}
				GraphQLFormat::Cbor => {
					return Err(RpcError::Thrown("Cbor is not yet supported".to_string()))
				}
			},
			Value::Object(mut o) => {
				let mut tmp = match o.remove("query") {
					Some(Value::Strand(s)) => async_graphql::Request::new(s),
					_ => return Err(RpcError::InvalidParams),
				};

				match o.remove("variables").or(o.remove("vars")) {
					Some(obj @ Value::Object(_)) => {
						let gql_vars = gql::schema::sql_value_to_gql_value(obj)
							.map_err(|_| RpcError::InvalidRequest)?;

						tmp = tmp.variables(async_graphql::Variables::from_value(gql_vars));
					}
					Some(_) => return Err(RpcError::InvalidParams),
					None => {}
				}

				match o.remove("operationName").or(o.remove("operation")) {
					Some(Value::Strand(s)) => tmp = tmp.operation_name(s),
					Some(_) => return Err(RpcError::InvalidParams),
					None => {}
				}

				tmp
			}
			_ => return Err(RpcError::InvalidParams),
		};

		let schema = self
			.graphql_schema_cache()
			.get_schema(self.session())
			.await
			.map_err(|e| RpcError::Thrown(e.to_string()))?;

		let res = schema.execute(req).await;

		let out = match pretty {
			true => {
				let mut buf = Vec::new();
				let formatter = serde_json::ser::PrettyFormatter::with_indent(b"    ");
				let mut ser = serde_json::Serializer::with_formatter(&mut buf, formatter);

				res.serialize(&mut ser).ok().and_then(|_| String::from_utf8(buf).ok())
			}
			false => serde_json::to_string(&res).ok(),
		}
		.ok_or(RpcError::Thrown("Serialization Error".to_string()))?;

		Ok(Value::Strand(out.into()))
	}

	// ------------------------------
	// Private methods
	// ------------------------------

	async fn query_inner(
		&self,
		query: Value,
		vars: Option<BTreeMap<String, Value>>,
	) -> Result<Vec<Response>, RpcError> {
		// If no live query handler force realtime off
		if !Self::LQ_SUPPORT && self.session().rt {
			return Err(RpcError::BadLQConfig);
		}
		// Execute the query on the database
		let res = match query {
			Value::Query(sql) => self.kvs().process(sql, self.session(), vars).await?,
			Value::Strand(sql) => self.kvs().execute(&sql, self.session(), vars).await?,
			_ => return Err(fail!("Unexpected query type: {query:?}").into()),
		};

		// Post-process hooks for web layer
		for response in &res {
			// This error should be unreachable because we shouldn't proceed if there's no handler
			self.handle_live_query_results(response).await;
		}
		// Return the result to the client
		Ok(res)
	}

	async fn handle_live_query_results(&self, res: &Response) {
		match &res.query_type {
			QueryType::Live => {
				if let Ok(Value::Uuid(lqid)) = &res.result {
					self.handle_live(&lqid.0).await;
				}
			}
			QueryType::Kill => {
				if let Ok(Value::Uuid(lqid)) = &res.result {
					self.handle_kill(&lqid.0).await;
				}
			}
			_ => {}
		}
	}
}
