use std::collections::HashMap;
use std::path::PathBuf;
use strum_macros::Display as StrumDisplay;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TypeIdent {
    Name(String),
    DefaultExport(),
    QualifiedName(Vec<String>),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TypeName {
    pub file: PathBuf,
    pub name: TypeIdent,
}

impl TypeName {
    pub fn default_export_for(file: PathBuf) -> TypeName {
        TypeName {
            file,
            name: TypeIdent::DefaultExport(),
        }
    }

    pub fn for_name<T: Into<PathBuf>>(file: T, name: &str) -> TypeName {
        TypeName {
            file: file.into(),
            name: TypeIdent::Name(name.to_string()),
        }
    }

    pub fn for_qualified_name(file: PathBuf, names: Vec<String>) -> TypeName {
        TypeName {
            file,
            name: TypeIdent::QualifiedName(names),
        }
    }

    pub fn to_name(&self) -> &str {
        match &self.name {
            TypeIdent::Name(n) => n,
            TypeIdent::QualifiedName(n) => n.last().expect("bad qualified name"),
            TypeIdent::DefaultExport() => "default",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum EnumValue {
    Str(String),
    Num(f64),
}

impl Eq for EnumValue {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnumMember {
    pub id: String,
    pub value: Option<EnumValue>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Param {
    pub name: String,
    pub type_info: TypeInfo,
    pub is_variadic: bool,
}

impl Param {
    fn resolve_names(
        &self,
        types_by_name_by_file: &HashMap<PathBuf, HashMap<TypeIdent, Type>>,
        type_params: &HashMap<String, TypeParamConfig>,
    ) -> Self {
        Param {
            name: self.name.clone(),
            type_info: self
                .type_info
                .resolve_names(types_by_name_by_file, type_params),
            is_variadic: self.is_variadic,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Func {
    pub type_params: Vec<(String, TypeParamConfig)>,
    pub params: Vec<Param>,
    pub return_type: Box<TypeInfo>,
    pub class_name: Option<TypeName>,
}

impl Func {
    fn resolve_names(
        &self,
        types_by_name_by_file: &HashMap<PathBuf, HashMap<TypeIdent, Type>>,
        type_params: &HashMap<String, TypeParamConfig>,
    ) -> Self {
        Func {
            type_params: self
                .type_params
                .iter()
                .map(|(n, t)| {
                    (
                        n.clone(),
                        t.resolve_names(types_by_name_by_file, type_params),
                    )
                })
                .collect(),
            params: self
                .params
                .iter()
                .map(|p| p.resolve_names(types_by_name_by_file, type_params))
                .collect(),
            return_type: Box::new(
                self.return_type
                    .resolve_names(types_by_name_by_file, type_params),
            ),
            class_name: self.class_name.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ctor {
    pub params: Vec<Param>,
}

impl Ctor {
    fn resolve_names(
        &self,
        types_by_name_by_file: &HashMap<PathBuf, HashMap<TypeIdent, Type>>,
        type_params: &HashMap<String, TypeParamConfig>,
    ) -> Self {
        Ctor {
            params: self
                .params
                .iter()
                .map(|p| p.resolve_names(types_by_name_by_file, type_params))
                .collect(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Member {
    Constructor(Ctor),
    Method(Func),
    Property(TypeInfo),
}

impl Member {
    fn resolve_names(
        &self,
        types_by_name_by_file: &HashMap<PathBuf, HashMap<TypeIdent, Type>>,
        type_params: &HashMap<String, TypeParamConfig>,
    ) -> Self {
        match self {
            Self::Constructor(ctor) => {
                Self::Constructor(ctor.resolve_names(types_by_name_by_file, type_params))
            }
            Self::Method(f) => Self::Method(f.resolve_names(types_by_name_by_file, type_params)),
            Self::Property(t) => {
                Self::Property(t.resolve_names(types_by_name_by_file, type_params))
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Indexer {
    pub readonly: bool,
    pub type_info: Box<TypeInfo>,
}

impl Indexer {
    fn resolve_names(
        &self,
        types_by_name_by_file: &HashMap<PathBuf, HashMap<TypeIdent, Type>>,
        type_params: &HashMap<String, TypeParamConfig>,
    ) -> Self {
        Indexer {
            readonly: self.readonly,
            type_info: Box::new(
                self.type_info
                    .resolve_names(types_by_name_by_file, type_params),
            ),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeRef {
    pub referent: TypeName,
    pub type_params: Vec<TypeInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NamespaceImport {
    Default { src: PathBuf },
    All { src: PathBuf },
    Named { src: PathBuf, name: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BaseClass {
    Unresolved(TypeRef),
    Resolved(TypeInfo),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Interface {
    pub indexer: Option<Indexer>,
    pub extends: Vec<BaseClass>,
    pub fields: HashMap<String, TypeInfo>,
    pub type_params: Vec<(String, TypeParamConfig)>,
    pub constructor: Option<Ctor>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeParamConfig {
    pub constraint: Option<TypeInfo>,
    pub default_type_arg: Option<TypeInfo>,
}

impl TypeParamConfig {
    fn resolve_names(
        &self,
        types_by_name_by_file: &HashMap<PathBuf, HashMap<TypeIdent, Type>>,
        type_params: &HashMap<String, TypeParamConfig>,
    ) -> Self {
        TypeParamConfig {
            constraint: self
                .constraint
                .as_ref()
                .map(|t| t.resolve_names(types_by_name_by_file, type_params)),
            default_type_arg: self
                .default_type_arg
                .as_ref()
                .map(|t| t.resolve_names(types_by_name_by_file, type_params)),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Class {
    pub super_class: Option<Box<TypeRef>>,
    pub members: HashMap<String, Member>,
    pub type_params: Vec<(String, TypeParamConfig)>,
    pub implements: Vec<TypeRef>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Intersection {
    pub types: Vec<TypeInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Union {
    pub types: Vec<TypeInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tuple {
    pub types: Vec<TypeInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Enum {
    pub members: Vec<EnumMember>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Alias {
    pub target: Box<TypeInfo>,
    pub type_params: Vec<(String, TypeParamConfig)>,
}

macro_rules! make_primitives {
    () => {};
    ($prim:ident) => {
        #[derive(Debug, Clone, PartialEq, Eq)]
        pub struct $prim();
    };
    ($prim:ident, $($rest:ident),* $(,)*) => {
        make_primitives!($prim);

        make_primitives!($($rest),*);
    };
}

make_primitives!(
    PrimitiveAny,
    PrimitiveNumber,
    PrimitiveObject,
    PrimitiveBoolean,
    PrimitiveBigInt,
    PrimitiveString,
    PrimitiveSymbol,
    PrimitiveVoid,
    PrimitiveUndefined,
    PrimitiveNull,
    BuiltinDate,
);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltinPromise {
    pub value_type: Box<TypeInfo>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LitNumber {
    pub n: f64,
}

impl Eq for LitNumber {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LitString {
    pub s: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LitBoolean {
    pub b: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, StrumDisplay)]
pub enum TypeInfo {
    Interface(Interface),
    Enum(Enum),
    Ref(TypeRef),
    Alias(Alias),
    PrimitiveAny(PrimitiveAny),
    PrimitiveNumber(PrimitiveNumber),
    PrimitiveObject(PrimitiveObject),
    PrimitiveBoolean(PrimitiveBoolean),
    PrimitiveBigInt(PrimitiveBigInt),
    PrimitiveString(PrimitiveString),
    PrimitiveSymbol(PrimitiveSymbol),
    PrimitiveVoid(PrimitiveVoid),
    PrimitiveUndefined(PrimitiveUndefined),
    PrimitiveNull(PrimitiveNull),
    BuiltinPromise(BuiltinPromise),
    BuiltinDate(BuiltinDate),
    Array { item_type: Box<TypeInfo> },
    Tuple(Tuple),
    Optional { item_type: Box<TypeInfo> },
    Union(Union),
    Intersection(Intersection),
    Mapped { value_type: Box<TypeInfo> },
    LitNumber(LitNumber),
    LitString(LitString),
    LitBoolean(LitBoolean),
    Func(Func),
    Constructor(Ctor),
    Class(Class),
    Var { type_info: Box<TypeInfo> },
    NamespaceImport(NamespaceImport),
    // TODO: we resolve TypeQuery in resolve_names. would much prefer not to leak TypeQuery into
    // our public interface because it complicates handling in future ir transforms.
    TypeQuery(TypeQuery),
}

impl TypeInfo {
    fn resolve_builtin(
        &self,
        referent: &TypeName,
        alias_type_params: &[TypeInfo],
        types_by_name_by_file: &HashMap<PathBuf, HashMap<TypeIdent, Type>>,
        type_params: &HashMap<String, TypeParamConfig>,
    ) -> Option<TypeInfo> {
        if referent.name == TypeIdent::Name("Array".to_string()) {
            assert_eq!(
                alias_type_params.len(),
                1,
                "expected 1 type param for Array"
            );
            return Some(TypeInfo::Array {
                item_type: Box::new(
                    alias_type_params
                        .first()
                        .as_ref()
                        .unwrap()
                        .resolve_names(types_by_name_by_file, type_params),
                ),
            });
        }

        if referent.name == TypeIdent::Name("Record".to_string()) {
            assert_eq!(
                alias_type_params.len(),
                2,
                "expected 2 type params for Record"
            );
            // TODO: do we care about key type?
            return Some(TypeInfo::Mapped {
                value_type: Box::new(
                    alias_type_params
                        .get(1)
                        .as_ref()
                        .unwrap()
                        .resolve_names(types_by_name_by_file, type_params),
                ),
            });
        }

        if referent.name == TypeIdent::Name("Date".to_string()) {
            return Some(TypeInfo::BuiltinDate(BuiltinDate()));
        }

        if referent.name == TypeIdent::Name("Function".to_string()) {
            return Some(TypeInfo::Func(Func {
                type_params: Default::default(),
                return_type: Box::new(TypeInfo::PrimitiveAny(PrimitiveAny())),
                class_name: None,
                params: vec![Param {
                    name: "args".to_string(),
                    type_info: TypeInfo::PrimitiveAny(PrimitiveAny()),
                    is_variadic: true,
                }],
            }));
        }

        if referent.name == TypeIdent::Name("Object".to_string()) {
            return Some(TypeInfo::Mapped {
                value_type: Box::new(TypeInfo::PrimitiveAny(PrimitiveAny())),
            });
        }

        if referent.name == TypeIdent::Name("Promise".to_string()) {
            return Some(TypeInfo::BuiltinPromise(BuiltinPromise {
                value_type: Box::new(
                    alias_type_params
                        .first()
                        .as_ref()
                        .map(|p| p.resolve_names(types_by_name_by_file, type_params))
                        .unwrap_or(TypeInfo::PrimitiveAny(PrimitiveAny())),
                ),
            }));
        }

        None
    }

    // TODO: resolve_names exists throughout this file exclusively to resolve builtin names...
    // is there a simpler way to do this without the full recursion?
    // perhaps move this to the flattened ir conversion when we have to walk our ast anyway?
    /// resolve_names resolves builtin names to their respective builtins and resolves TypeQuery to
    /// the corresponding type
    fn resolve_names(
        &self,
        types_by_name_by_file: &HashMap<PathBuf, HashMap<TypeIdent, Type>>,
        type_params: &HashMap<String, TypeParamConfig>,
    ) -> Self {
        match self {
            Self::Interface(Interface {
                indexer,
                extends,
                fields,
                type_params: iface_type_params,
                constructor,
            }) => {
                let iface_type_params =
                    resolve_type_params(types_by_name_by_file, type_params, iface_type_params);
                let our_type_params = extend_type_params(type_params, &iface_type_params);
                Self::Interface(Interface {
                    indexer: indexer
                        .as_ref()
                        .map(|i| i.resolve_names(types_by_name_by_file, &our_type_params)),
                    extends: extends
                        .iter()
                        .map(|e| {
                            // TODO: no reason to have BaseClass - just use a ref
                            if let BaseClass::Unresolved(tn) = e {
                                BaseClass::Resolved(TypeInfo::Ref(tn.clone()))
                            } else {
                                e.clone()
                            }
                        })
                        .collect(),
                    fields: fields
                        .iter()
                        .map(|(n, t)| {
                            (
                                n.to_string(),
                                t.resolve_names(types_by_name_by_file, &our_type_params),
                            )
                        })
                        .collect(),
                    type_params: iface_type_params,
                    constructor: constructor
                        .as_ref()
                        .map(|c| c.resolve_names(types_by_name_by_file, &our_type_params)),
                })
            }
            Self::Ref(TypeRef {
                referent,
                type_params: alias_type_params,
            }) => lookup_type(types_by_name_by_file, referent)
                .map(|_| {
                    // look up the type just to make sure it exists, we get a builtin if it doesn't exist below
                    self.clone()
                })
                .or_else(|| {
                    if let TypeIdent::Name(n) = &referent.name {
                        type_params.get(n).map(|_| self.clone())
                    } else {
                        None
                    }
                })
                .or_else(|| {
                    self.resolve_builtin(
                        referent,
                        alias_type_params,
                        types_by_name_by_file,
                        type_params,
                    )
                })
                .or_else(|| {
                    println!(
                        "can't resolve, {:?}, {:?}",
                        self,
                        types_by_name_by_file.get(&referent.file)
                    );
                    None
                })
                .expect("can't resolve alias"),
            Self::Alias(a) => Self::Alias(a.clone()),
            Self::Array { item_type } => Self::Array {
                item_type: Box::new(item_type.resolve_names(types_by_name_by_file, type_params)),
            },
            Self::Optional { item_type } => Self::Optional {
                item_type: Box::new(item_type.resolve_names(types_by_name_by_file, type_params)),
            },
            Self::Union(Union { types }) => Self::Union(Union {
                types: types
                    .iter()
                    .map(|t| t.resolve_names(types_by_name_by_file, type_params))
                    .collect(),
            }),
            Self::Intersection(Intersection { types }) => Self::Intersection(Intersection {
                types: types
                    .iter()
                    .map(|t| t.resolve_names(types_by_name_by_file, type_params))
                    .collect(),
            }),
            Self::Tuple(Tuple { types }) => Self::Tuple(Tuple {
                types: types
                    .iter()
                    .map(|t| t.resolve_names(types_by_name_by_file, type_params))
                    .collect(),
            }),
            Self::Mapped { value_type } => Self::Mapped {
                value_type: Box::new(value_type.resolve_names(types_by_name_by_file, type_params)),
            },
            Self::Func(Func {
                params,
                type_params: fn_type_params,
                return_type,
                class_name,
            }) => {
                let fn_type_params =
                    resolve_type_params(types_by_name_by_file, type_params, fn_type_params);
                let tps = extend_type_params(type_params, &fn_type_params);
                Self::Func(Func {
                    params: params
                        .iter()
                        .map(|p| Param {
                            name: p.name.to_string(),
                            is_variadic: p.is_variadic,
                            type_info: p.type_info.resolve_names(types_by_name_by_file, &tps),
                        })
                        .collect(),
                    return_type: Box::new(
                        return_type.resolve_names(types_by_name_by_file, type_params),
                    ),
                    class_name: class_name.clone(),
                    type_params: fn_type_params,
                })
            }
            Self::Constructor(Ctor { params }) => Self::Constructor(Ctor {
                params: params
                    .iter()
                    .map(|p| Param {
                        name: p.name.to_string(),
                        is_variadic: p.is_variadic,
                        type_info: p
                            .type_info
                            .resolve_names(types_by_name_by_file, type_params),
                    })
                    .collect(),
            }),
            Self::Class(Class {
                members,
                super_class,
                type_params: class_type_params,
                implements,
            }) => {
                let class_type_params =
                    resolve_type_params(types_by_name_by_file, type_params, class_type_params);
                let tps = extend_type_params(type_params, &class_type_params);

                Self::Class(Class {
                    super_class: super_class.clone(),
                    members: members
                        .iter()
                        .map(|(n, m)| (n.to_string(), m.resolve_names(types_by_name_by_file, &tps)))
                        .collect(),
                    type_params: class_type_params,
                    implements: implements.clone(),
                })
            }
            Self::Var { type_info } => Self::Var {
                type_info: Box::new(type_info.resolve_names(types_by_name_by_file, type_params)),
            },
            Self::Enum(_) => self.clone(),
            Self::PrimitiveAny(PrimitiveAny()) => self.clone(),
            Self::PrimitiveNumber(PrimitiveNumber()) => self.clone(),
            Self::PrimitiveObject(PrimitiveObject()) => self.clone(),
            Self::PrimitiveBoolean(PrimitiveBoolean()) => self.clone(),
            Self::PrimitiveBigInt(PrimitiveBigInt()) => self.clone(),
            Self::PrimitiveString(PrimitiveString()) => self.clone(),
            Self::PrimitiveSymbol(PrimitiveSymbol()) => self.clone(),
            Self::PrimitiveVoid(PrimitiveVoid()) => self.clone(),
            Self::PrimitiveUndefined(PrimitiveUndefined()) => self.clone(),
            Self::PrimitiveNull(PrimitiveNull()) => self.clone(),
            Self::LitNumber(_) => self.clone(),
            Self::LitString(_) => self.clone(),
            Self::LitBoolean(_) => self.clone(),
            Self::BuiltinDate(BuiltinDate()) => self.clone(),
            Self::BuiltinPromise(_) => self.clone(),
            Self::NamespaceImport { .. } => self.clone(),
            Self::TypeQuery(TypeQuery::LookupRef(tr)) => {
                lookup_type(types_by_name_by_file, &tr.referent)
                    .map(|t| {
                        let t = &t.info;
                        if let Self::Var { type_info } = t {
                            std::ops::Deref::deref(type_info).clone()
                        } else {
                            t.clone()
                        }
                    })
                    .expect("expected target of type query to exit")
            }
        }
    }
}

fn lookup_type<'a>(
    types_by_name_by_file: &'a HashMap<PathBuf, HashMap<TypeIdent, Type>>,
    referent: &TypeName,
) -> Option<&'a Type> {
    types_by_name_by_file
        .get(&referent.file)
        .and_then(|types_by_name| {
            let n = if let TypeIdent::QualifiedName(qn) = &referent.name {
                TypeIdent::Name(
                    qn.first()
                        .expect("must have a name in a qualified name")
                        .to_string(),
                )
            } else {
                referent.name.clone()
            };

            types_by_name.get(&n)
        })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeQuery {
    LookupRef(TypeRef),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Type {
    pub name: TypeName,
    pub is_exported: bool,
    pub info: TypeInfo,
}

impl Type {
    pub fn resolve_names(
        &self,
        types_by_name_by_file: &HashMap<PathBuf, HashMap<TypeIdent, Type>>,
    ) -> Self {
        Self {
            name: self.name.clone(),
            is_exported: self.is_exported,
            info: self
                .info
                .resolve_names(types_by_name_by_file, &Default::default()),
        }
    }
}

fn extend_type_params(
    a: &HashMap<String, TypeParamConfig>,
    b: &[(String, TypeParamConfig)],
) -> HashMap<String, TypeParamConfig> {
    a.iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .chain(b.iter().map(|(k, v)| (k.clone(), v.clone())))
        .collect()
}

fn resolve_type_params(
    types_by_name_by_file: &HashMap<PathBuf, HashMap<TypeIdent, Type>>,
    type_params: &HashMap<String, TypeParamConfig>,
    our_type_params: &[(String, TypeParamConfig)],
) -> Vec<(String, TypeParamConfig)> {
    our_type_params
        .iter()
        .map(|(n, c)| {
            (
                n.clone(),
                c.resolve_names(types_by_name_by_file, type_params),
            )
        })
        .collect()
}
