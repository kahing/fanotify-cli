use std::error;
use std::fmt::{self, Debug, Display, Formatter};
use std::marker::PhantomData;

pub trait EnumValues {
    type Enum: Debug;

    fn values() -> Vec<Self::Enum>;
}

pub struct CEnumParseError<T: EnumValues>(String, PhantomData<T>);

impl<T> CEnumParseError<T>
where
    T: EnumValues + Send + Sync,
{
    pub fn new<S: AsRef<str> + Sized>(name: S) -> CEnumParseError<T> {
        CEnumParseError::<T>(name.as_ref().into(), PhantomData)
    }
}

impl<T> error::Error for CEnumParseError<T> where T: EnumValues + Send + Sync {}

impl<T> Debug for CEnumParseError<T>
where
    T: EnumValues,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "invalid value: {}, options: {}",
            self.0,
            T::values()
                .iter()
                .map(|e| format!("{:?}", e))
                .collect::<Vec<String>>()
                .join(", ")
        )
    }
}

impl<T> Display for CEnumParseError<T>
where
    T: EnumValues,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self)
    }
}

macro_rules! c_enum {
    (
	$(enum $name:ident {
	    $($flag:ident),* $(,)*
	})*
    ) => (
     $(
	 #[repr(u64)]
	 #[derive(Copy, Clone, Debug)]
	 #[allow(non_camel_case_types)]
	 enum $name {
	     $($flag = libc::$flag),*
	 }

	 impl FromStr for $name {
	     type Err = $crate::c_enum::CEnumParseError<$name>;

	     fn from_str(s: &str) -> Result<Self, Self::Err> {
		 match s {
		     $( stringify!($flag) => Ok($name::$flag), )*
		     _ => Err($crate::c_enum::CEnumParseError::new(s))
		 }
	     }
	 }

	 impl AsRef<str> for $name {
	     fn as_ref(&self) -> &str {
		 match self {
		     $( $name::$flag => stringify!($flag), )*
		 }
	     }
	 }

	 impl $crate::c_enum::EnumValues for $name {
	     type Enum = $name;

	     fn values() -> Vec<$name> {
		 vec![$($name::$flag),*]
	     }
	 }

     )*
    );
}
