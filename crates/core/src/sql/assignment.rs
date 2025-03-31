use crate::err::Error;
use crate::sql::array::Array;
use crate::sql::idiom::Idiom;
use crate::sql::object::Object;
use crate::sql::operator::Operator;
use crate::sql::value::Value;
use revision::revisioned;
use serde::{Deserialize, Serialize};
use std::fmt;

pub(crate) const TOKEN: &str = "$surrealdb::private::sql::Assignment";

#[revisioned(revision = 1)]
#[derive(Clone, Debug, Eq, PartialEq, PartialOrd, Serialize, Deserialize, Hash)]
#[serde(rename = "$surrealdb::private::sql::Assignment")]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
#[non_exhaustive]
pub struct Assignment {
	pub(crate) l: Idiom,
	pub(crate) o: Operator,
	pub(crate) r: Value,
}

impl From<(Idiom, Operator, Value)> for Assignment {
	fn from(tuple: (Idiom, Operator, Value)) -> Assignment {
		Assignment {
			l: tuple.0,
			o: tuple.1,
			r: tuple.2,
		}
	}
}

impl TryFrom<(Value, Value, Value)> for Assignment {
	type Error = Error;

	fn try_from(tuple: (Value, Value, Value)) -> Result<Self, Self::Error> {
		let idiom = tuple.1.to_idiom();
		let operator = match tuple.1 {
			Value::Strand(o) => match o.as_str() {
				"=" => Operator::Equal,
				"+=" => Operator::Inc,
				"-=" => Operator::Dec,
				"+?=" => Operator::Ext,
				_ => return Err(Error::InvalidOperator(o.to_string())),
			},
			o => return Err(Error::try_from(o.to_string())),
		};

		Ok(Assignment {
			l: idiom,
			o: operator,
			r: tuple.2,
		})
	}
}

impl TryFrom<Array> for Assignment {
	type Error = Error;

	fn try_from(a: Array) -> Result<Self, Self::Error> {
		if a.len() != 3 {
			return Err(Error::TryFrom(a.to_string(), "Assignment"));
		}
		match Assignment::try_from((a[0].clone(), a[1].clone(), a[2].clone())) {
			Ok(assignment) => Ok(assignment),
			Err(e) => return Err(e),
		}
	}
}

impl TryFrom<Object> for Assignment {
	type Error = Error;

	fn try_from(a: Object) -> Result<Self, Self::Error> {
		if !(a.contains_key("l") && a.contains_key("o") && a.contains_key("r")) {
			return Err(Error::TryFrom(a.to_string(), "Assignment"));
		}
		match Assignment::try_from((
			a.get("l").cloned().ok_or_else(|| Error::TryFrom("l".to_string(), "Assignment"))?,
			a.get("o").cloned().ok_or_else(|| Error::TryFrom("o".to_string(), "Assignment"))?,
			a.get("r").cloned().ok_or_else(|| Error::TryFrom("r".to_string(), "Assignment"))?,
		)) {
			Ok(assignment) => Ok(assignment),
			Err(e) => return Err(e),
		}
	}
}

impl TryFrom<Value> for Assignment {
	type Error = Error;

	fn try_from(value: Value) -> Result<Self, Self::Error> {
		match value {
			Value::Object(o) => match Assignment::try_from(o) {
				Ok(assignment) => Ok(assignment),
				Err(e) => return Err(e),
			},
			Value::Array(a) => match Assignment::try_from(a) {
				Ok(assignment) => Ok(assignment),
				Err(e) => return Err(e),
			},
			_ => return Err(Error::TryFrom(value.to_string(), "Assignment")),
		}
	}
}

impl fmt::Display for Assignment {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "{} {} {}", self.l, self.o, self.r)
	}
}
