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

macro_rules! bit_as(
    (
	$trait:ident, $lhs:ident $op:tt $rhs:ident = $out:ident
    ) => (
	 impl $trait<$rhs> for $lhs {
	     type Output = $out;

	     fn bitand(self, rhs: $rhs) -> Self::Output {
		 self as $out & rhs as $out
	     }
	 }
    );
);

macro_rules! bit_as_assoc(
    (
	$trait:ident, $lhs:ident $op:tt $rhs:ident = $out:ident
    ) => (
	bit_as!($trait, $lhs $op $rhs = $out);
	bit_as!($trait, $rhs $op $lhs = $out);
    );
);

macro_rules! __c_enum_impl {
    (
	$name:ident, $ty:ident, $($flag:ident),* $(,)*
    ) => (
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

	impl std::fmt::Display for $name {
	    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "{}", stringify!($name))
	    }
	}

	bit_as_assoc!(BitAnd, $name & $ty = $ty);
    );
}

macro_rules! c_enum {
    (
	$(enum $name:ident {
	    $($flag:ident),* $(,)*
	})*
    ) => (
	c_enum! {
	    $(enum(u64) $name {
		$($flag),*
	    })*
	}
    );

    (
	$(enum(u32) $name:ident {
	    $($flag:ident),* $(,)*
	})*
    ) => (
	$(
	    #[repr(u32)]
	    #[derive(Copy, Clone, Debug)]
	    #[allow(non_camel_case_types)]
	    enum $name {
		$($flag = libc::$flag),*
	    }

	    __c_enum_impl!($name, u32, $($flag),*);

     )*
    );

    (
	$(enum(u64) $name:ident {
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

	    __c_enum_impl!($name, u64, $($flag),*);

     )*
    );
}
