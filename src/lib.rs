extern crate proc_macro;

// TODO: aliases should point to modules
// TODO: when generating code, use include_str! to make the compiler think we have a dependency on
// any ts files we use so we recompile when they do:
// https://github.com/rustwasm/wasm-bindgen/pull/1295/commits/b762948456617ee263de8e43b3636bd3a4d1da75

use proc_macro::TokenStream;
use proc_macro2::{TokenStream as TokenStream2};
use quote::{quote, format_ident, ToTokens, TokenStreamExt};
use serde_json::Value;
use std::collections::{hash_map::Entry, HashMap};
use std::ffi::OsStr;
use std::fs::File;
use std::path::{Path, PathBuf, Component};
use std::convert::{From, Into};
use std::rc::Rc;
use std::cell::RefCell;
use swc_common::{sync::Lrc, SourceMap};
use swc_ecma_ast::*;
use swc_ecma_parser::{lexer::Lexer, Parser, StringInput, Syntax, TsConfig};
use syn::parse::{Parse, ParseStream};
use syn::punctuated::Punctuated;
use syn::{parse_macro_input, LitStr, Result as ParseResult, Token, parse_str as parse_syn_str};
use unicode_xid::UnicodeXID;

#[proc_macro]
pub fn import_ts(input: TokenStream) -> TokenStream {
    let import_args = parse_macro_input!(input as ImportArgs);
    let mods = import_args
        .modules
        .iter()
        .map(|module| {
            let tt = TsTypes::try_new(&module).expect("tt error");
            use std::borrow::Borrow;
            let mod_def: ModDef = tt.types_by_name_by_file.borrow().into();
            let mod_toks = quote! { #mod_def };
            // let mod_toks = quote! { };

            let mut file = std::fs::File::create("output.rs").expect("failed to create file");
            std::io::Write::write_all(&mut file, mod_toks.to_string().as_bytes()).expect("failed to write");

            mod_toks
        })
        .collect::<Vec<TokenStream2>>();
    (quote! {
        #(#mods)*
    })
    .into()
}

struct ImportArgs {
    modules: Vec<String>,
}

impl Parse for ImportArgs {
    fn parse(input: ParseStream) -> ParseResult<Self> {
        let modules = Punctuated::<LitStr, Token![,]>::parse_terminated(input)?;
        Ok(ImportArgs {
            modules: modules.into_iter().map(|m| m.value()).collect(),
        })
    }
}

fn typings_module_resolver(import_path: &Path, pkg: &Value) -> std::io::Result<PathBuf> {
    let types_rel_path = pkg
        .as_object()
        .ok_or(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "Bad package.json (expected top-level object) found in {}",
                import_path.display()
            ),
        ))?
        .get("types")
        .ok_or(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "Bad package.json (expected 'types' property) found in {}",
                import_path.display()
            ),
        ))?
        .as_str()
        .ok_or(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "Bad package.json (expected 'types' to be a string) found in {}",
                import_path.display()
            ),
        ))?;

    let types_path = import_path.join(types_rel_path);
    if types_path.is_file() {
        Ok(types_path)
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!(
                "Package.json in {} specified non-existent file for types, {}",
                import_path.display(),
                types_path.display()
            ),
        ))
    }
}

fn path_with_ext_appended(path: &Path, ext: &str) -> PathBuf {
    path.with_file_name(format!(
        "{}.{}",
        path.file_name()
            .unwrap_or(OsStr::new(""))
            .to_str()
            .unwrap_or(""),
        ext
    ))
}

fn get_file_with_any_ext(path: &Path) -> std::io::Result<PathBuf> {
    let exts = vec!["d.ts", "ts", "tsx", "js", "jsx", "json"];
    exts.iter()
        .map(|ext| path_with_ext_appended(path, ext))
        .find(|path_with_ext| path_with_ext.is_file())
        .ok_or(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!(
                "Could not find module with any extension, {}",
                path.display()
            ),
        ))
}

fn get_ts_path(
    module_base: Option<PathBuf>,
    import: &str,
    module_resolver: &dyn Fn(&Path, &Value) -> std::io::Result<PathBuf>,
) -> std::io::Result<PathBuf> {
    let cwd = module_base.unwrap_or(std::env::current_dir()?);
    let mut path = cwd.clone();
    let abs_import_path = Path::new(import);

    if abs_import_path.is_absolute() {
        if abs_import_path.is_dir() {
            get_file_with_any_ext(&abs_import_path.join("index"))
        } else {
            get_file_with_any_ext(&abs_import_path)
        }
    } else if import.starts_with(".") {
        let import_path = path.join(import);
        if import_path.is_dir() {
            get_file_with_any_ext(&import_path.join("index"))
        } else {
            get_file_with_any_ext(&import_path)
        }
    } else {
        loop {
            let possible_node_modules = path.join("node_modules");
            if possible_node_modules.is_dir() {
                let import_path = possible_node_modules.join(import);
                if import_path.is_dir() {
                    // module path
                    // check package.json for typings
                    let pkg_json_path = import_path.join("package.json");
                    let file = File::open(&pkg_json_path)?;
                    let pkg: Value = serde_json::from_reader(file)?;
                    break module_resolver(&import_path, &pkg);
                } else if import_path.exists() {
                    // must be a module + file path
                    break Ok(import_path);
                } else {
                    // check with different file extensions
                    match get_file_with_any_ext(&import_path) {
                        Ok(import_path) => break Ok(import_path),
                        Err(err) => (), // fall through so that we iterate up the directory tree, looking for a higher-level node_modules folder
                    };
                }
            }

            if !path.pop() {
                break Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!(
                        "Could not find node_modules directory starting at {}",
                        cwd.display()
                    ),
                ));
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum TypeIdent {
    Name(String),
    DefaultExport(),
    QualifiedName(Vec<String>),
}

#[derive(Debug, Clone)]
struct TypeName {
    file: PathBuf,
    name: TypeIdent,
}

impl TypeName {
    fn default_export_for(file: PathBuf) -> TypeName {
        TypeName {
            file,
            name: TypeIdent::DefaultExport(),
        }
    }

    fn for_name<T: Into<PathBuf>>(file: T, name: &str) -> TypeName {
        TypeName {
            file: file.into(),
            name: TypeIdent::Name(name.to_string()),
        }
    }

    fn for_qualified_name(file: PathBuf, names: Vec<String>) -> TypeName {
        TypeName {
            file,
            name: TypeIdent::QualifiedName(names),
        }
    }

    fn to_name(&self) -> &str {
        match &self.name {
            TypeIdent::Name(n) => n,
            TypeIdent::QualifiedName(n) => n.last().expect("bad qualified name"),
            TypeIdent::DefaultExport() => "default",
        }
    }
}

#[derive(Debug, Clone)]
struct EnumMember {
    id: String,
    value: Option<String>, // TODO: really a string | number
}

#[derive(Debug, Clone)]
struct Param {
    name: String,
    type_info: TypeInfo,
    is_variadic: bool,
}

#[derive(Debug, Clone)]
struct Func {
    type_params: HashMap<String, TypeInfo>,
    params: Vec<Param>,
    return_type: Box<TypeInfo>,
}

#[derive(Debug, Clone)]
enum Member {
    Constructor(),
    Method(),
    Property(),
}

impl Member {
    fn resolve_names(
        &self,
        types_by_name_by_file: &HashMap<PathBuf, HashMap<TypeIdent, Type>>,
        type_params: &HashMap<String, TypeInfo>,
    ) -> Self {
        self.clone() // TODO
    }
}

#[derive(Debug, Clone)]
struct Indexer {
    readonly: bool,
    type_info: Box<TypeInfo>,
}

impl Indexer {
    fn resolve_names(
        &self,
        types_by_name_by_file: &HashMap<PathBuf, HashMap<TypeIdent, Type>>,
        type_params: &HashMap<String, TypeInfo>,
    ) -> Self {
        Indexer {
            readonly: self.readonly,
            type_info: Box::new(
                self.type_info
                    .resolve_names(&types_by_name_by_file, &type_params),
            ),
        }
    }
}

#[derive(Debug, Clone)]
enum TypeInfo {
    Interface {
        indexer: Option<Indexer>,
        fields: HashMap<String, TypeInfo>,
    },
    Enum {
        members: Vec<EnumMember>,
    },
    Ref {
        referent: TypeName,
        type_params: Vec<TypeInfo>,
    },
    Alias {
        target: TypeName,
    },
    PrimitiveAny {},
    PrimitiveNumber {},
    PrimitiveObject {},
    PrimitiveBoolean {},
    PrimitiveBigInt {},
    PrimitiveString {},
    PrimitiveSymbol {},
    PrimitiveVoid {},
    PrimitiveUndefined {},
    PrimitiveNull {},
    BuiltinPromise {
        value_type: Box<TypeInfo>,
    },
    BuiltinDate {},
    Array {
        item_type: Box<TypeInfo>,
    },
    Optional {
        item_type: Box<TypeInfo>,
    },
    Union {
        types: Vec<TypeInfo>,
    },
    Intersection {
        types: Vec<TypeInfo>,
    },
    Mapped {
        value_type: Box<TypeInfo>,
    },
    LitNumber {
        n: f64,
    },
    LitString {
        s: String,
    },
    LitBoolean {
        b: bool,
    },
    Func(Func),
    Constructor {
        params: Vec<Param>,
        return_type: Box<TypeInfo>,
    },
    Class {
        members: HashMap<String, Member>,
    },
    Var {
        type_info: Box<TypeInfo>,
    },
    GenericType {
        name: String,
        constraint: Box<TypeInfo>,
    },
    NamespaceImport(NamespaceImport),
}

#[derive(Debug, Clone)]
enum NamespaceImport {
    Default {
        src: PathBuf,
    },
    All {
        src: PathBuf,
    },
    Named {
        src: PathBuf,
        name: String
    },
}

impl TypeInfo {
    fn resolve_builtin(
        &self,
        referent: &TypeName,
        alias_type_params: &Vec<TypeInfo>,
        types_by_name_by_file: &HashMap<PathBuf, HashMap<TypeIdent, Type>>,
        type_params: &HashMap<String, TypeInfo>,
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
                        .resolve_names(&types_by_name_by_file, &type_params),
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
                        .resolve_names(&types_by_name_by_file, &type_params),
                ),
            });
        }

        if referent.name == TypeIdent::Name("Date".to_string()) {
            return Some(TypeInfo::BuiltinDate {});
        }

        if referent.name == TypeIdent::Name("Function".to_string()) {
            return Some(TypeInfo::Func(Func {
                type_params: Default::default(),
                return_type: Box::new(TypeInfo::PrimitiveAny {}),
                params: vec![Param {
                    name: "args".to_string(),
                    type_info: TypeInfo::PrimitiveAny {},
                    is_variadic: true,
                }],
            }));
        }

        if referent.name == TypeIdent::Name("Object".to_string()) {
            return Some(TypeInfo::Mapped {
                value_type: Box::new(TypeInfo::PrimitiveAny {}),
            });
        }

        if referent.name == TypeIdent::Name("Promise".to_string()) {
            return Some(TypeInfo::BuiltinPromise {
                value_type: Box::new(
                    alias_type_params
                        .first()
                        .as_ref()
                        .map(|p| p.resolve_names(&types_by_name_by_file, &type_params))
                        .unwrap_or(TypeInfo::PrimitiveAny {}),
                ),
            });
        }

        None
    }

    fn resolve_names(
        &self,
        types_by_name_by_file: &HashMap<PathBuf, HashMap<TypeIdent, Type>>,
        type_params: &HashMap<String, TypeInfo>,
    ) -> Self {
        match self {
            Self::Interface { indexer, fields } => Self::Interface {
                indexer: indexer
                    .as_ref()
                    .map(|i| i.resolve_names(&types_by_name_by_file, &type_params)),
                fields: fields
                    .iter()
                    .map(|(n, t)| {
                        (
                            n.to_string(),
                            t.resolve_names(&types_by_name_by_file, &type_params),
                        )
                    })
                    .collect(),
            },
            Self::Ref {
                referent,
                type_params: alias_type_params,
            } => {
                if let TypeIdent::Name(ref name) = &referent.name {
                    if let Some(constraint) = type_params.get(name) {
                        return Self::GenericType {
                            name: name.to_string(),
                            constraint: Box::new(constraint.clone()),
                        };
                    }
                }

                types_by_name_by_file
                    .get(&referent.file)
                    .and_then(|types_by_name| match &referent.name {
                        TypeIdent::QualifiedName(path) => {
                            Some(TypeInfo::Alias {
                                target: referent.clone(),
                            })
                        },
                        n @ TypeIdent::DefaultExport() => types_by_name.get(&n).map(|t| t.info.clone()),
                        n @ TypeIdent::Name(..) => types_by_name.get(&n).map(|t| t.info.clone()),
                    })
                    .or_else(|| {
                        self.resolve_builtin(
                            &referent,
                            &alias_type_params,
                            &types_by_name_by_file,
                            &type_params,
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
                    .expect("can't resolve alias")
            },
            Self::Alias { target } => Self::Alias {
                target: target.clone(),
            },
            Self::Array { item_type } => Self::Array {
                item_type: Box::new(item_type.resolve_names(&types_by_name_by_file, &type_params)),
            },
            Self::Optional { item_type } => Self::Optional {
                item_type: Box::new(item_type.resolve_names(&types_by_name_by_file, &type_params)),
            },
            Self::Union { types } => Self::Union {
                types: types
                    .iter()
                    .map(|t| t.resolve_names(&types_by_name_by_file, &type_params))
                    .collect(),
            },
            Self::Intersection { types } => Self::Intersection {
                types: types
                    .iter()
                    .map(|t| t.resolve_names(&types_by_name_by_file, &type_params))
                    .collect(),
            },
            Self::Mapped { value_type } => Self::Mapped {
                value_type: Box::new(
                    value_type.resolve_names(&types_by_name_by_file, &type_params),
                ),
            },
            Self::Func(Func {
                params,
                type_params: fn_type_params,
                return_type,
            }) => {
                let tps = {
                    let mut tps = type_params.clone();
                    tps.extend(fn_type_params.clone().into_iter());
                    tps
                };
                Self::Func(Func {
                    type_params: fn_type_params.clone(),
                    params: params
                        .iter()
                        .map(|p| Param {
                            name: p.name.to_string(),
                            is_variadic: p.is_variadic.clone(),
                            type_info: p.type_info.resolve_names(&types_by_name_by_file, &tps),
                        })
                        .collect(),
                    return_type: Box::new(
                        return_type.resolve_names(&types_by_name_by_file, &type_params),
                    ),
                })
            }
            Self::Constructor {
                params,
                return_type,
            } => Self::Constructor {
                params: params
                    .iter()
                    .map(|p| Param {
                        name: p.name.to_string(),
                        is_variadic: p.is_variadic.clone(),
                        type_info: p
                            .type_info
                            .resolve_names(&types_by_name_by_file, &type_params),
                    })
                    .collect(),
                return_type: Box::new(
                    return_type.resolve_names(&types_by_name_by_file, &type_params),
                ),
            },
            Self::Class { members } => Self::Class {
                members: members
                    .iter()
                    .map(|(n, m)| {
                        (
                            n.to_string(),
                            m.resolve_names(&types_by_name_by_file, &type_params),
                        )
                    })
                    .collect(),
            },
            Self::Var { type_info } => Self::Var {
                type_info: Box::new(type_info.resolve_names(&types_by_name_by_file, &type_params)),
            },
            Self::GenericType { name, constraint } => Self::GenericType {
                name: name.to_string(),
                constraint: Box::new(
                    constraint.resolve_names(&types_by_name_by_file, &type_params),
                ),
            },
            Self::Enum { .. } => self.clone(),
            Self::PrimitiveAny {} => self.clone(),
            Self::PrimitiveNumber {} => self.clone(),
            Self::PrimitiveObject {} => self.clone(),
            Self::PrimitiveBoolean {} => self.clone(),
            Self::PrimitiveBigInt {} => self.clone(),
            Self::PrimitiveString {} => self.clone(),
            Self::PrimitiveSymbol {} => self.clone(),
            Self::PrimitiveVoid {} => self.clone(),
            Self::PrimitiveUndefined {} => self.clone(),
            Self::PrimitiveNull {} => self.clone(),
            Self::LitNumber { .. } => self.clone(),
            Self::LitString { .. } => self.clone(),
            Self::LitBoolean { .. } => self.clone(),
            Self::BuiltinDate {} => self.clone(),
            Self::BuiltinPromise { .. } => self.clone(),
            Self::NamespaceImport { .. } => self.clone(),
        }
    }
}

#[derive(Debug, Clone)]
struct Type {
    name: TypeName,
    is_exported: bool,
    info: TypeInfo,
}

impl Type {
    fn resolve_names(
        &self,
        types_by_name_by_file: &HashMap<PathBuf, HashMap<TypeIdent, Type>>,
    ) -> Self {
        Self {
            name: self.name.clone(),
            is_exported: self.is_exported,
            info: self
                .info
                .resolve_names(&types_by_name_by_file, &Default::default()),
        }
    }
}

#[derive(Debug, Clone)]
struct MutModDef {
    name: proc_macro2::Ident,
    types: Vec<Type>,
    children: Vec<Rc<RefCell<MutModDef>>>,
}

impl MutModDef {
    fn to_mod_def(self) -> ModDef {
        ModDef {
            name: self.name,
            types: self.types,
            children: self.children.into_iter().map(move |c| Rc::try_unwrap(c).expect("Rc still borrowed").into_inner().to_mod_def()).collect(),
        }
    }

    fn add_child_mod(&mut self, mod_name: proc_macro2::Ident, types: Vec<Type>) -> Rc<RefCell<MutModDef>> {
        if let Some(child) = self
            .children
            .iter()
            .find(|c| c.borrow().name == mod_name) {
            let child = child.clone();
            child.borrow_mut().types.extend(types);
            child
        } else {
            let child = Rc::new(RefCell::new(MutModDef {
                name: mod_name,
                types ,
                children: Default::default()
            }));
            self.children.push(child.clone());
            child
        }
    }
}

#[derive(Debug, Clone)]
struct ModDef {
    name: proc_macro2::Ident,
    types: Vec<Type>,
    children: Vec<ModDef>,
}

trait ToModPathIter {
    fn to_mod_path_iter(&self) -> Box<dyn Iterator<Item = proc_macro2::Ident>>;
}

impl ToModPathIter for Path {
    fn to_mod_path_iter(&self) -> Box<dyn Iterator<Item = proc_macro2::Ident>> {
        Box::new(
            self
                .canonicalize()
                .expect("canonicalize failed")
                .components()
                .filter_map(|c| match c {
                    Component::Normal(s) => Some(s.to_string_lossy()),
                    _ => None,
                })
                .rev()
                .take_while(|p| p != "node_modules")
                .map(|p| p.as_ref().to_string())
                .collect::<Vec<String>>()
                .into_iter()
                .rev()
                .map(|n| to_ns_name(&n))
        )
    }
}

impl ToModPathIter for TypeIdent {
    fn to_mod_path_iter(&self) -> Box<dyn Iterator<Item = proc_macro2::Ident>> {
        if let TypeIdent::QualifiedName(names) = &self {
            Box::new(
                (&names[..names.len() - 1]).to_vec().into_iter().map(|n| to_ident(&n))
            )
        } else {
            Box::new(vec![].into_iter())
        }
    }
}

impl ToModPathIter for TypeName {
    fn to_mod_path_iter(&self) -> Box<dyn Iterator<Item = proc_macro2::Ident>> {
        Box::new(
            self.file.to_mod_path_iter().chain(self.name.to_mod_path_iter())
        )
    }
}

// TODO: maybe don't make "index" namespaces and put their types in the parent
impl From<&HashMap<PathBuf, HashMap<TypeIdent, Type>>> for ModDef {
    fn from(types_by_name_by_file: &HashMap<PathBuf, HashMap<TypeIdent, Type>>) -> Self {
        let root = Rc::new(RefCell::new(MutModDef {
            name: to_ns_name("root"),
            types: Default::default(),
            children: Default::default()
        }));

        types_by_name_by_file.iter().for_each(|(path, types_by_name)| {
            // given a path like /.../node_modules/a/b/c, we fold over
            // [a, b, c].
            // given a path like /a/b/c (without a node_modules), we fold
            // over [a, b, c].
            let mod_path = path.to_mod_path_iter().collect::<Vec<proc_macro2::Ident>>();
            let last_idx = mod_path.len() - 1;

            mod_path
                .iter()
                .enumerate()
                .fold(
                    root.clone(),
                    move |parent, (i, mod_name)| {
                        let mut parent = parent.borrow_mut();
                        let types = if i == last_idx {
                            types_by_name.values().cloned().collect::<Vec<Type>>()
                        } else {
                            Default::default()
                        };
                        parent.add_child_mod(mod_name.clone(), types)
                    }
                );

            types_by_name
                .iter()
                .filter_map(|(name, typ)| {
                    if let TypeIdent::QualifiedName(names) = name {
                        Some((name.to_mod_path_iter().collect::<Vec<proc_macro2::Ident>>(), typ))
                    } else {
                        None
                    }
                }).for_each(|(names, typ)| {
                    let last_idx = mod_path.len() + names.len() - 1;
                    mod_path
                        .iter()
                        .chain(names.iter())
                        .enumerate()
                        .fold(
                            root.clone(),
                            move |parent, (i, mod_name)| {
                                let mut parent = parent.borrow_mut();
                                let types = if i == last_idx {
                                    vec![typ.clone()]
                                } else {
                                    Default::default()
                                };
                                parent.add_child_mod(mod_name.clone(), types)
                            }
                        );
                });
        });

        Rc::try_unwrap(root).unwrap().into_inner().to_mod_def()
    }
}

fn to_ident(s: &str) -> proc_macro2::Ident {
    // make sure we have valid characters
    let mut chars = s.chars();
    let first: String = chars.by_ref().take(1).map(|first| {
        if UnicodeXID::is_xid_start(first) && first != '_' {
            first.to_string()
        } else {
            "".to_string()
        }
    }).collect();

    let rest: String = chars.map(|c| {
        if UnicodeXID::is_xid_continue(c) {
            c
        } else {
            '_'
        }
    }).collect();

    // now, make sure we have a valid rust identifier (no keyword collissions)
    let mut full_ident = first + &rest;
    while parse_syn_str::<syn::Ident>(&full_ident).is_err() {
        full_ident += "_";
    }

    format_ident!("{}", &full_ident)
}

fn camel_case_ident(s: &str) -> proc_macro2::Ident {
    let s = s.to_uppercase();
    to_ident(&s)
}

fn to_ns_name(ns: &str) -> proc_macro2::Ident {
    let ns = ns.to_lowercase();
    to_ident(ns.trim_end_matches(".d.ts").trim_end_matches(".ts"))
}

impl ToTokens for ModDef {
    fn to_tokens(&self, toks: &mut TokenStream2) {
        let mod_name = &self.name;
        let types = &self.types;
        let children = &self.children;

        // TODO: would be nice to do something like use super::super::... as ts_bindgen_root and be
        // able to refer to it in future use clauses. just need to get the nesting level here
        let our_toks = quote! {
            #[cfg(target_arch = "wasm32")]
            pub mod #mod_name {
                use wasm_bindgen::prelude::*;

                #(#types)*

                #(#children)*
            }
        };

        toks.append_all(our_toks);
    }
}

impl ToTokens for EnumMember {
    fn to_tokens(&self, toks: &mut TokenStream2) {
        let id = camel_case_ident(&self.id);
        let our_toks = {
            if let Some(value) = &self.value {
                quote! {
                    #id = #value
                }
            } else {
                quote! {
                    #id
                }
            }
        };
        toks.append_all(our_toks);
    }
}

trait ToNsPath<T: ?Sized> {
    // TODO: would love to return a generic ToTokens...
    fn to_ns_path(&self, current_mod: &T) -> TokenStream2;
}

impl<T, U> ToNsPath<T> for U where T: ToModPathIter, U: ToModPathIter + ?Sized {
    fn to_ns_path(&self, current_mod: &T) -> TokenStream2 {
        let ns_len = current_mod.to_mod_path_iter().count();
        let mut use_path = vec![format_ident!("super"); ns_len];
        use_path.extend(
            self.to_mod_path_iter()
        );
        quote! {
            #(#use_path)::*
        }
    }
}

fn to_unique_ident(mut desired: String, taken: &Fn(&str) -> bool) -> proc_macro2::Ident {
    while taken(&desired) {
        desired += "_";
    }

    to_ident(&desired)
}

impl ToTokens for Type {
    fn to_tokens(&self, toks: &mut TokenStream2) {
        let js_name = self.name.to_name();
        let name = camel_case_ident(&js_name);

        let our_toks = match &self.info {
            TypeInfo::Interface {
                indexer,
                fields,
            } => {
                let mut field_toks = fields.iter().map(|(js_field_name, typ)| {
                    let field_name = to_ident(js_field_name);
                    quote! {
                        #[serde(rename = #js_field_name)]
                        pub #field_name: String
                    }
                }).collect::<Vec<TokenStream2>>();

                if let Some(Indexer { readonly, type_info }) = &indexer {
                    let extra_fields_name = to_unique_ident("extra_fields".to_string(), &|x| fields.contains_key(x));

                    field_toks.push(
                        quote! {
                            #[serde(flatten)]
                            pub #extra_fields_name: std::collections::HashMap<String, String>
                        }
                    );
                }

                quote! {
                    #[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
                    struct #name {
                        #(#field_toks),*
                    }
                }
            },
            TypeInfo::Enum { members, } => {
                quote! {
                    #[wasm_bindgen]
                    #[derive(Clone, Debug)]
                    pub enum #name {
                        #(#members),*
                    }
                }
            },
            TypeInfo::Ref { .. } => panic!("should not have any aliases at token generation time"),
            TypeInfo::Alias { target } => {
                // we super::super our way up to root and then append the target namespace
                let use_path = target.to_ns_path(&self.name);

                quote! {
                    use #use_path as #name;
                }
            },
            TypeInfo::PrimitiveAny {} => {
                quote! {
                    pub type #name = JsValue;
                }
            },
            TypeInfo::PrimitiveNumber {} => {
                quote! {
                    pub type #name = f64;
                }
            },
            TypeInfo::PrimitiveObject {} => {
                quote! {
                    std::collections::HashMap<String, JsValue>
                }
            },
            TypeInfo::PrimitiveBoolean {} => {
                quote! {
                    pub type #name = bool;
                }
            },
            TypeInfo::PrimitiveBigInt {} => {
                // TODO
                quote! {
                    pub type #name = u64;
                }
            },
            TypeInfo::PrimitiveString {} => {
                quote! {
                    pub type #name = String;
                }
            },
            TypeInfo::PrimitiveSymbol {} => panic!("how do we handle symbols"),
            TypeInfo::PrimitiveVoid {} => {
                quote! {}
            },
            TypeInfo::PrimitiveUndefined {} => {
                quote! {}
            },
            /*
            TypeInfo::PrimitiveNull {},
            TypeInfo::BuiltinPromise {
                value_type: Box<TypeInfo>,
            },
            TypeInfo::BuiltinDate {},
            TypeInfo::Array {
                item_type: Box<TypeInfo>,
            },
            TypeInfo::Optional {
                item_type: Box<TypeInfo>,
            },
            TypeInfo::Union {
                types: Vec<TypeInfo>,
            },
            TypeInfo::Intersection {
                types: Vec<TypeInfo>,
            },
            TypeInfo::Mapped {
                value_type: Box<TypeInfo>,
            },
            TypeInfo::LitNumber {
                n: f64,
            },
            TypeInfo::LitString {
                s: String,
            },
            TypeInfo::LitBoolean {
                b: bool,
            },
            TypeInfo::Func(Func),
            TypeInfo::Constructor {
                params: Vec<Param>,
                return_type: Box<TypeInfo>,
            },
            TypeInfo::Class {
                members: HashMap<String, Member>,
            },
            TypeInfo::Var {
                type_info: Box<TypeInfo>,
            },
            TypeInfo::GenericType {
                name: String,
                constraint: Box<TypeInfo>,
            },*/
            TypeInfo::NamespaceImport(NamespaceImport::All { src }) => {
                let ns = src.as_path().to_ns_path(&self.name);
                let vis = if self.is_exported {
                    let vis = format_ident!("pub");
                    quote! { #vis }
                } else {
                    quote! {}
                };

                quote! {
                    #vis use #ns as #name;
                }
            },
            TypeInfo::NamespaceImport(NamespaceImport::Default { src }) => {
                let ns = src.as_path().to_ns_path(&self.name);
                let vis = if self.is_exported {
                    let vis = format_ident!("pub");
                    quote! { #vis }
                } else {
                    quote! {}
                };
                let default_export = to_ident("default");

                quote! {
                    #vis use #ns::#default_export as #name;
                }
            },
            TypeInfo::NamespaceImport(NamespaceImport::Named { src, name: item_name }) => {
                let ns = src.as_path().to_ns_path(&self.name);
                let vis = if self.is_exported {
                    let vis = format_ident!("pub");
                    quote! { #vis }
                } else {
                    quote! {}
                };
                let item_name = to_ident(item_name);

                quote! {
                    #vis use #ns::#item_name as #name;
                }
            },
            _ => { quote! { }},
        };

        toks.append_all(our_toks);
    }
}

#[derive(Default, Debug)]
struct TsTypes {
    types_by_name_by_file: HashMap<PathBuf, HashMap<TypeIdent, Type>>,
    namespace_stack: Vec<Vec<String>>,
}

impl TsTypes {
    fn try_new(module_name: &str) -> Result<TsTypes, swc_ecma_parser::error::Error> {
        let mut tt: TsTypes = Default::default();
        tt.process_module(None, module_name)?;

        let mut resolved_types_by_name_by_file: HashMap<PathBuf, HashMap<TypeIdent, Type>> =
            HashMap::new();
        for (file, types_by_name) in &tt.types_by_name_by_file {
            let resolved = types_by_name
                .iter()
                .map(|(n, typ)| (n.clone(), typ.resolve_names(&tt.types_by_name_by_file)))
                .collect();
            resolved_types_by_name_by_file.insert(file.clone(), resolved);
        }

        tt.types_by_name_by_file = resolved_types_by_name_by_file;

        Ok(tt)
    }

    fn load_module(&mut self, ts_path: &Path) -> Result<Module, swc_ecma_parser::error::Error> {
        let cm: Lrc<SourceMap> = Default::default();
        let fm = cm.load_file(ts_path).expect("Can't load file");
        let lexer = Lexer::new(
            Syntax::Typescript(TsConfig {
                tsx: true,
                decorators: true,
                dynamic_import: true,
                dts: true,
                no_early_errors: true,
                import_assertions: true,
            }),
            Default::default(),
            StringInput::from(&*fm),
            None,
        );

        let mut parser = Parser::new_from(lexer);
        let module = parser.parse_typescript_module()?;
        if ts_path.to_string_lossy().contains("hello.d.ts") {
            println!("MOD!, {:?}", module);
        }

        Ok(module)
    }

    fn process_module_item(&mut self, ts_path: &Path, item: &ModuleItem) {
        match item {
            ModuleItem::ModuleDecl(decl) => self.process_module_decl(&ts_path, &decl),
            ModuleItem::Stmt(stmt) => self.process_stmt(&ts_path, &stmt),
        }
    }

    fn process_module_items(&mut self, ts_path: &Path, items: &Vec<ModuleItem>) {
        for item in items {
            self.process_module_item(&ts_path, &item);
        }
    }

    fn process_module(
        &mut self,
        module_base: Option<PathBuf>,
        module_name: &str,
    ) -> Result<PathBuf, swc_ecma_parser::error::Error> {
        let ts_path = get_ts_path(module_base, &module_name, &typings_module_resolver)
            .expect("TODO: Need to convert this exception type")
            .canonicalize()
            .expect("TODO: Need to convert this exception type");

        match self.types_by_name_by_file.entry(ts_path.clone()) {
            Entry::Occupied(_) => return Ok(ts_path),
            Entry::Vacant(v) => {
                v.insert(Default::default());
                ()
            }
        }

        let module = self.load_module(&ts_path)?;
        self.process_module_items(&ts_path, &module.body);

        Ok(ts_path)
    }

    fn set_type_for_name_for_file(&mut self, file: &Path, name: TypeIdent, typ: Type) {
        match self.namespace_stack.last() {
            Some(ns) => {
                let mut ns = ns.clone();
                match name {
                    TypeIdent::Name(s) => {
                        ns.push(s);
                    }
                    TypeIdent::DefaultExport() => panic!("default export within namespace"),
                    TypeIdent::QualifiedName(name) => {
                        let mut name = name.clone();
                        ns.append(&mut name);
                    }
                }

                self.types_by_name_by_file
                    .entry(file.to_path_buf())
                    .and_modify(|names_to_types: &mut HashMap<TypeIdent, Type>| {
                        names_to_types.insert(TypeIdent::QualifiedName(ns), typ);
                    });
            }
            None => {
                self.types_by_name_by_file
                    .entry(file.to_path_buf())
                    .and_modify(|names_to_types: &mut HashMap<TypeIdent, Type>| {
                        names_to_types.insert(name.clone(), typ);
                    });
            }
        }
    }

    fn process_import_decl(
        &mut self,
        ts_path: &Path,
        ImportDecl {
            specifiers, src, ..
        }: &ImportDecl,
    ) {
        let base = ts_path.parent().expect("All files must have a parent");
        let import = src.value.to_string();

        let file = self
            .process_module(Some(base.to_path_buf()), &import)
            .expect("failed to process module");

        specifiers
            .into_iter()
            .for_each(|specifier| match specifier {
                ImportSpecifier::Named(ImportNamedSpecifier {
                    local, imported, ..
                }) => {
                    self.set_type_for_name_for_file(
                        ts_path,
                        TypeIdent::Name(local.sym.to_string()),
                        Type {
                            name: TypeName::for_name(ts_path, &local.sym.to_string()),
                            is_exported: false,
                            info: TypeInfo::NamespaceImport(NamespaceImport::Named {
                                src: file.to_path_buf(),
                                name: imported.as_ref().unwrap_or(local).sym.to_string(),
                            }),
                        },
                    );
                }
                ImportSpecifier::Default(ImportDefaultSpecifier { local, .. }) => {
                    self.set_type_for_name_for_file(
                        ts_path,
                        TypeIdent::Name(local.sym.to_string()),
                        Type {
                            name: TypeName::for_name(ts_path, &local.sym.to_string()),
                            is_exported: false,
                            info: TypeInfo::NamespaceImport(NamespaceImport::Default {
                                src: file.to_path_buf(),
                            })
                        },
                    );
                }
                ImportSpecifier::Namespace(ImportStarAsSpecifier { local, .. }) => {
                    self.set_type_for_name_for_file(
                        ts_path,
                        TypeIdent::Name(local.sym.to_string()),
                        Type {
                            name: TypeName::for_name(ts_path, &local.sym.to_string()),
                            is_exported: false,
                            info: TypeInfo::NamespaceImport(NamespaceImport::All {
                                src: file.to_path_buf(),
                            })
                        },
                    );
                }
            })
    }

    fn import_namespace(
        &mut self,
        ts_path: &Path,
        import_from: &Path,
        ns: &Ident,
        should_export: bool,
    ) {
        let full_ns = {
            let mut full_ns = self
                .namespace_stack
                .last()
                .unwrap_or(&Default::default())
                .clone();
            full_ns.push(ns.sym.to_string());
            full_ns
        };
        self.namespace_stack.push(full_ns);

        let t_by_n: HashMap<String, Type> = self
            .types_by_name_by_file
            .get(import_from)
            .expect("should have processed file already")
            .iter()
            .filter(|(n, t)| t.is_exported)
            .filter_map(|(n, t)| match n {
                TypeIdent::Name(name) => Some((name.to_string(), t.clone())),
                _ => None,
            })
            .collect();

        t_by_n.into_iter().for_each(|(name, mut typ)| {
            typ.is_exported = should_export;
            typ.info = TypeInfo::Alias {
                target: typ.name.clone(),
            };

            self.set_type_for_name_for_file(ts_path, TypeIdent::Name(name), typ);
        });

        self.namespace_stack.pop();
    }

    fn process_export_all(&mut self, ts_path: &Path, export_all: &ExportAll) {
        let s = export_all.src.value.to_string();
        let dir = ts_path.parent().expect("All files must have a parent");

        let file = self
            .process_module(Some(dir.to_path_buf()), &s)
            .expect("failed to process module");

        let type_name = format!("*EXPORT_ALL*{}*", file.to_string_lossy());

        let to_export = self
            .types_by_name_by_file
            .get(&file)
            .expect("should have processed file already")
            .iter()
            .filter(|(n, t)| t.is_exported)
            .filter_map(|(n, t)| match n {
                n @ TypeIdent::Name(_) => Some((n.clone(), t.clone())),
                _ => None,
            })
            .collect::<HashMap<TypeIdent, Type>>();

        to_export.into_iter().for_each(|(name, typ)| {
            self.set_type_for_name_for_file(ts_path, name, typ);
        });
    }

    fn qualified_name_to_str_vec(&mut self, ts_path: &Path, qn: &TsQualifiedName) -> Vec<String> {
        let mut en = TsEntityName::TsQualifiedName(Box::new(qn.clone()));
        let mut names = Vec::new();

        loop {
            match en {
                TsEntityName::TsQualifiedName(qn) => {
                    names.push(qn.right.sym.to_string());
                    en = qn.left;
                }
                TsEntityName::Ident(Ident { sym, .. }) => {
                    names.push(sym.to_string());
                    break;
                }
            }
        }

        names.reverse();
        names
    }

    fn qualified_name_to_type_name(&mut self, ts_path: &Path, qn: &TsQualifiedName) -> TypeName {
        let name_path = self.qualified_name_to_str_vec(&ts_path, qn);
        TypeName::for_qualified_name(ts_path.to_path_buf(), name_path)
    }

    fn process_type_ref(
        &mut self,
        ts_path: &Path,
        TsTypeRef {
            type_name,
            type_params,
            ..
        }: &TsTypeRef,
    ) -> TypeInfo {
        match type_name {
            TsEntityName::Ident(Ident { sym, .. }) => TypeInfo::Ref {
                referent: TypeName::for_name(ts_path.to_path_buf(), &sym.to_string()),
                type_params: type_params
                    .as_ref()
                    .map(|tps| {
                        tps.params
                            .iter()
                            .map(|tp| self.process_type(ts_path, tp))
                            .collect()
                    })
                    .unwrap_or(Default::default()),
            },
            TsEntityName::TsQualifiedName(qn) => TypeInfo::Ref {
                referent: self.qualified_name_to_type_name(ts_path, qn),
                type_params: type_params
                    .as_ref()
                    .map(|tps| {
                        tps.params
                            .iter()
                            .map(|tp| self.process_type(ts_path, tp))
                            .collect()
                    })
                    .unwrap_or(Default::default()),
            },
        }
    }

    fn process_keyword_type(
        &mut self,
        ts_path: &Path,
        TsKeywordType { kind, .. }: &TsKeywordType,
    ) -> TypeInfo {
        match kind {
            TsKeywordTypeKind::TsAnyKeyword => TypeInfo::PrimitiveAny {},
            TsKeywordTypeKind::TsUnknownKeyword => panic!("unknown keyword"),
            TsKeywordTypeKind::TsNumberKeyword => TypeInfo::PrimitiveNumber {},
            TsKeywordTypeKind::TsObjectKeyword => TypeInfo::PrimitiveObject {},
            TsKeywordTypeKind::TsBooleanKeyword => TypeInfo::PrimitiveBoolean {},
            TsKeywordTypeKind::TsBigIntKeyword => TypeInfo::PrimitiveBigInt {},
            TsKeywordTypeKind::TsStringKeyword => TypeInfo::PrimitiveString {},
            TsKeywordTypeKind::TsSymbolKeyword => TypeInfo::PrimitiveSymbol {},
            TsKeywordTypeKind::TsVoidKeyword => TypeInfo::PrimitiveVoid {},
            TsKeywordTypeKind::TsUndefinedKeyword => TypeInfo::PrimitiveUndefined {},
            TsKeywordTypeKind::TsNullKeyword => TypeInfo::PrimitiveNull {},
            TsKeywordTypeKind::TsNeverKeyword => panic!("never keyword"),
            TsKeywordTypeKind::TsIntrinsicKeyword => panic!("intrinsic keyword"),
        }
    }

    fn process_array_type(
        &mut self,
        ts_path: &Path,
        TsArrayType { elem_type, .. }: &TsArrayType,
    ) -> TypeInfo {
        TypeInfo::Array {
            item_type: Box::new(self.process_type(ts_path, elem_type)),
        }
    }

    fn process_optional_type(
        &mut self,
        ts_path: &Path,
        TsOptionalType { type_ann, .. }: &TsOptionalType,
    ) -> TypeInfo {
        TypeInfo::Optional {
            item_type: Box::new(self.process_type(ts_path, type_ann)),
        }
    }

    fn process_union_type(
        &mut self,
        ts_path: &Path,
        TsUnionType { types, .. }: &TsUnionType,
    ) -> TypeInfo {
        TypeInfo::Union {
            types: types
                .iter()
                .map(|t| self.process_type(ts_path, t))
                .collect(),
        }
    }

    fn process_intersection_type(
        &mut self,
        ts_path: &Path,
        TsIntersectionType { types, .. }: &TsIntersectionType,
    ) -> TypeInfo {
        TypeInfo::Intersection {
            types: types
                .iter()
                .map(|t| self.process_type(ts_path, t))
                .collect(),
        }
    }

    fn process_type_lit(
        &mut self,
        ts_path: &Path,
        TsTypeLit { members, .. }: &TsTypeLit,
    ) -> TypeInfo {
        if members.len() != 1 || !members.first().expect("no members").is_ts_index_signature() {
            panic!("Bad type lit, {:?}, in {:?}", members, ts_path);
        }

        let mem = members.first().expect("no members for mapped type");
        if let TsTypeElement::TsIndexSignature(index_sig) = mem {
            TypeInfo::Mapped {
                value_type: Box::new(
                    self.process_type(
                        ts_path,
                        &index_sig
                            .type_ann
                            .as_ref()
                            .expect("Need a type for a mapped type")
                            .type_ann,
                    ),
                ),
            }
        } else {
            panic!("bad members for mapped type, {:?}", members);
        }
    }

    fn process_literal_type(
        &mut self,
        ts_path: &Path,
        TsLitType { lit, .. }: &TsLitType,
    ) -> TypeInfo {
        match lit {
            TsLit::Number(n) => TypeInfo::LitNumber { n: n.value },
            TsLit::Str(s) => TypeInfo::LitString {
                s: s.value.to_string(),
            },
            TsLit::Bool(b) => TypeInfo::LitBoolean { b: b.value },
            TsLit::BigInt(n) => panic!("we don't support literal bigints yet"),
            TsLit::Tpl(t) => panic!("we don't support template literals yet"),
        }
    }

    fn process_params(&mut self, ts_path: &Path, params: &Vec<TsFnParam>) -> Vec<Param> {
        params
            .iter()
            .map(|p| match p {
                TsFnParam::Ident(id_param) => Param {
                    name: id_param.id.sym.to_string(),
                    is_variadic: false,
                    type_info: id_param
                        .type_ann
                        .as_ref()
                        .map(|p_type| self.process_type(ts_path, &p_type.type_ann))
                        .unwrap_or(TypeInfo::PrimitiveAny {}),
                },
                _ => panic!("we only handle ident params"),
            })
            .collect()
    }

    fn process_fn_type_params(
        &mut self,
        ts_path: &Path,
        type_params: &Option<TsTypeParamDecl>,
    ) -> HashMap<String, TypeInfo> {
        type_params
            .as_ref()
            .map(|params| {
                params
                    .params
                    .iter()
                    .map(|p| {
                        (
                            p.name.sym.to_string(),
                            p.constraint
                                .as_ref()
                                .map(|c| self.process_type(ts_path, &c))
                                .unwrap_or(TypeInfo::PrimitiveAny {}),
                        )
                    })
                    .collect()
            })
            .unwrap_or(Default::default())
    }

    fn process_fn_type(
        &mut self,
        ts_path: &Path,
        TsFnType {
            type_ann,
            params,
            type_params,
            span,
            ..
        }: &TsFnType,
    ) -> TypeInfo {
        TypeInfo::Func(Func {
            type_params: self.process_fn_type_params(ts_path, type_params),
            params: self.process_params(ts_path, params),
            return_type: Box::new(self.process_type(ts_path, &type_ann.type_ann)),
        })
    }

    fn process_ctor_type(
        &mut self,
        ts_path: &Path,
        TsConstructorType {
            type_ann,
            params,
            type_params,
            ..
        }: &TsConstructorType,
    ) -> TypeInfo {
        TypeInfo::Constructor {
            params: self.process_params(ts_path, params),
            return_type: Box::new(self.process_type(ts_path, &type_ann.type_ann)),
        }
    }

    fn process_type_predicate(
        &mut self,
        ts_path: &Path,
        TsTypePredicate {
            param_name,
            type_ann,
            ..
        }: &TsTypePredicate,
    ) -> TypeInfo {
        TypeInfo::Func(Func {
            type_params: Default::default(),
            params: vec![Param {
                name: match param_name {
                    TsThisTypeOrIdent::Ident(ident) => ident.sym.to_string(),
                    TsThisTypeOrIdent::TsThisType(this) => "this".to_string(),
                },
                is_variadic: false,
                type_info: type_ann
                    .as_ref()
                    .map(|t| self.process_type(ts_path, &t.type_ann))
                    .unwrap_or(TypeInfo::PrimitiveAny {}),
            }],
            return_type: Box::new(TypeInfo::PrimitiveBoolean {}),
        })
    }

    fn process_type(&mut self, ts_path: &Path, ts_type: &TsType) -> TypeInfo {
        match ts_type {
            TsType::TsTypeRef(type_ref) => self.process_type_ref(ts_path, type_ref),
            TsType::TsKeywordType(keyword_type) => self.process_keyword_type(ts_path, keyword_type),
            TsType::TsArrayType(array_type) => self.process_array_type(ts_path, array_type),
            TsType::TsOptionalType(opt_type) => self.process_optional_type(ts_path, opt_type),
            TsType::TsUnionOrIntersectionType(TsUnionOrIntersectionType::TsUnionType(
                union_type,
            )) => self.process_union_type(ts_path, union_type),
            TsType::TsUnionOrIntersectionType(TsUnionOrIntersectionType::TsIntersectionType(
                isect_type,
            )) => self.process_intersection_type(ts_path, isect_type),
            TsType::TsTypeLit(type_lit) => self.process_type_lit(ts_path, type_lit),
            TsType::TsLitType(lit_type) => self.process_literal_type(ts_path, lit_type),
            TsType::TsParenthesizedType(TsParenthesizedType { type_ann, .. }) => {
                self.process_type(ts_path, &type_ann)
            }
            TsType::TsFnOrConstructorType(TsFnOrConstructorType::TsFnType(f)) => {
                self.process_fn_type(ts_path, &f)
            }
            TsType::TsFnOrConstructorType(TsFnOrConstructorType::TsConstructorType(ctor)) => {
                self.process_ctor_type(ts_path, &ctor)
            }
            TsType::TsTypePredicate(pred) => self.process_type_predicate(ts_path, &pred),
            // TODO: more cases
            _ => {
                println!("MISSING {:?} {:?}", ts_path, ts_type);
                TypeInfo::Ref {
                    referent: TypeName::default_export_for(ts_path.to_path_buf()),
                    type_params: Default::default(),
                }
            }
        }
    }

    fn prop_key_to_name(&self, expr: &Expr) -> Option<String> {
        match expr {
            Expr::Lit(lit) => match lit {
                Lit::Str(s) => Some(s.value.to_string()),
                _ => {
                    println!("We only handle string properties. Received {:?}", lit);
                    None
                }
            },
            Expr::Ident(Ident { sym, .. }) => Some(sym.to_string()),
            _ => {
                println!(
                    "We only handle literal and identifier properties. Received {:?}",
                    expr
                );
                None
            }
        }
    }

    fn process_ts_interface(
        &mut self,
        ts_path: &Path,
        TsInterfaceDecl {
            id,
            type_params,
            extends,
            body,
            ..
        }: &TsInterfaceDecl,
    ) -> Type {
        Type {
            name: TypeName::for_name(ts_path, &id.sym.to_string()),
            is_exported: false,
            info: TypeInfo::Interface {
                indexer: body.body.iter().find_map(|el| match el {
                    TsTypeElement::TsIndexSignature(TsIndexSignature {
                        readonly,
                        type_ann,
                        params,
                        ..
                    }) => {
                        if params.len() != 1 {
                            panic!("indexing signatures should only have 1 param");
                        }

                        Some(Indexer {
                            readonly: readonly.clone(),
                            type_info: Box::new(match params.first().unwrap() {
                                TsFnParam::Ident(ident) => ident
                                    .type_ann
                                    .as_ref()
                                    .map(|t| self.process_type(ts_path, &t.type_ann))
                                    .unwrap_or(TypeInfo::PrimitiveAny {}),
                                _ => panic!("we only support ident indexers"),
                            }),
                        })
                    }
                    _ => None,
                }),
                fields: body
                    .body
                    .iter()
                    .filter_map(|el| match el {
                        TsTypeElement::TsPropertySignature(TsPropertySignature {
                            key,
                            type_ann,
                            optional,
                            ..
                        }) => Some((
                            self.prop_key_to_name(key).expect("bad prop key"),
                            type_ann
                                .as_ref()
                                .map(|t| {
                                    let item_type = self.process_type(ts_path, &t.type_ann);
                                    if *optional {
                                        TypeInfo::Optional {
                                            item_type: Box::new(item_type),
                                        }
                                    } else {
                                        item_type
                                    }
                                })
                                .unwrap_or(TypeInfo::PrimitiveAny {}),
                        )),
                        TsTypeElement::TsMethodSignature(TsMethodSignature {
                            key,
                            params,
                            type_ann,
                            type_params,
                            ..
                        }) => Some((
                            self.prop_key_to_name(key).expect("bad method key"),
                            TypeInfo::Func(Func {
                                params: params
                                    .iter()
                                    .map(|param| match param {
                                        TsFnParam::Ident(ident) => Param {
                                            name: ident.id.sym.to_string(),
                                            is_variadic: false,
                                            type_info: ident
                                                .type_ann
                                                .as_ref()
                                                .map(|t| self.process_type(ts_path, &t.type_ann))
                                                .unwrap_or(TypeInfo::PrimitiveAny {}),
                                        },
                                        _ => panic!("we only support ident params for methods"),
                                    })
                                    .collect(),
                                type_params: self.process_fn_type_params(ts_path, &type_params),
                                return_type: Box::new(
                                    type_ann
                                        .as_ref()
                                        .map(|t| self.process_type(ts_path, &t.type_ann))
                                        .unwrap_or(TypeInfo::PrimitiveAny {}),
                                ),
                            }),
                        )),
                        TsTypeElement::TsIndexSignature(TsIndexSignature { .. }) => None,
                        // TODO: add other variants
                        _ => {
                            println!("unknown_variant: {:?}", el);
                            None
                        }
                    })
                    .collect(),
            },
        }
    }

    fn process_ts_enum(
        &mut self,
        ts_path: &Path,
        TsEnumDecl { id, members, .. }: &TsEnumDecl,
    ) -> Type {
        Type {
            name: TypeName::for_name(ts_path, &id.sym.to_string()),
            is_exported: false,
            info: TypeInfo::Enum {
                members: members
                    .iter()
                    .map(|TsEnumMember { id, init, .. }| {
                        EnumMember {
                            id: match id {
                                TsEnumMemberId::Ident(ident) => ident.sym.to_string(),
                                TsEnumMemberId::Str(s) => s.value.to_string(),
                            },
                            value: init.as_ref().and_then(|v| {
                                match &**v {
                                    Expr::Lit(l) => match l {
                                        Lit::Str(s) => Some(s.value.to_string()),
                                        // TODO: might need to capture numbers too
                                        _ => None,
                                    },
                                    _ => panic!("enums may only be initialized with lits"),
                                }
                            }),
                        }
                    })
                    .collect(),
            },
        }
    }

    fn process_ts_alias(
        &mut self,
        ts_path: &Path,
        TsTypeAliasDecl {
            id,
            type_params,
            type_ann,
            ..
        }: &TsTypeAliasDecl,
    ) -> Type {
        let type_info = self.process_type(ts_path, &*type_ann);
        Type {
            name: TypeName::for_name(ts_path, &id.sym.to_string()),
            is_exported: false,
            info: type_info,
        }
    }

    fn process_prop_name(&mut self, ts_path: &Path, prop_name: &PropName) -> String {
        match prop_name {
            PropName::Ident(ident) => ident.sym.to_string(),
            PropName::Str(s) => s.value.to_string(),
            PropName::Num(n) => n.value.to_string(),
            _ => panic!("We only support ident, str, and num property names"),
        }
    }

    fn process_class(
        &mut self,
        ts_path: &Path,
        Class {
            body,
            super_class,
            type_params,
            super_type_params,
            ..
        }: &Class,
    ) -> TypeInfo {
        TypeInfo::Class {
            members: body
                .iter()
                .filter_map(|member| match member {
                    ClassMember::Constructor(ctor) => Some((
                        self.process_prop_name(ts_path, &ctor.key),
                        Member::Constructor(),
                    )),
                    ClassMember::Method(method) => Some((
                        self.process_prop_name(ts_path, &method.key),
                        Member::Method(),
                    )),
                    ClassMember::PrivateMethod(_) => None,
                    ClassMember::ClassProp(prop) => Some((
                        self.prop_key_to_name(&prop.key)
                            .expect("we only handle some prop key types"),
                        Member::Property(),
                    )),
                    ClassMember::PrivateProp(_) => None,
                    ClassMember::TsIndexSignature(_) => None,
                    ClassMember::Empty(_) => None,
                })
                .collect(),
        }
    }

    fn process_class_type(
        &mut self,
        ts_path: &Path,
        ClassDecl { ident, class, .. }: &ClassDecl,
    ) -> Type {
        Type {
            name: TypeName::for_name(ts_path, &ident.sym.to_string()),
            is_exported: false,
            info: self.process_class(ts_path, class),
        }
    }

    fn process_var(&mut self, ts_path: &Path, VarDeclarator { name, .. }: &VarDeclarator) -> Type {
        match name {
            Pat::Ident(BindingIdent { id, type_ann }) => Type {
                name: TypeName::for_name(ts_path, &id.sym.to_string()),
                is_exported: false,
                info: TypeInfo::Var {
                    type_info: Box::new(
                        type_ann
                            .as_ref()
                            .map(|t| self.process_type(ts_path, &t.type_ann))
                            .unwrap_or(TypeInfo::PrimitiveAny {}),
                    ),
                },
            },
            _ => panic!("We only support regular identifier variables"),
        }
    }

    fn process_raw_params(
        &mut self,
        ts_path: &Path,
        params: &Vec<swc_ecma_ast::Param>,
    ) -> Vec<Param> {
        params
            .iter()
            .map(|p| match &p.pat {
                Pat::Ident(id_param) => Param {
                    name: id_param.id.sym.to_string(),
                    is_variadic: false,
                    type_info: id_param
                        .type_ann
                        .as_ref()
                        .map(|p_type| self.process_type(ts_path, &p_type.type_ann))
                        .unwrap_or(TypeInfo::PrimitiveAny {}),
                },
                Pat::Rest(RestPat { arg, type_ann, .. }) => match &**arg {
                    Pat::Ident(id_param) => Param {
                        name: id_param.id.sym.to_string(),
                        is_variadic: false,
                        type_info: type_ann
                            .as_ref()
                            .map(|t| self.process_type(ts_path, &t.type_ann))
                            .unwrap_or(TypeInfo::PrimitiveAny {}),
                    },
                    _ => {
                        println!("found rest param arg {:?}", &arg);
                        panic!("we only handle idents in rest patterns");
                    }
                },
                _ => {
                    println!("found param, {:?}", &p);
                    panic!("bad params")
                }
            })
            .collect()
    }

    fn process_fn_decl(
        &mut self,
        ts_path: &Path,
        FnDecl {
            ident,
            function:
                Function {
                    params,
                    return_type,
                    type_params,
                    ..
                },
            ..
        }: &FnDecl,
    ) -> Type {
        Type {
            name: TypeName::for_name(ts_path, &ident.sym.to_string()),
            is_exported: false,
            info: TypeInfo::Func(Func {
                params: self.process_raw_params(ts_path, params),
                type_params: self.process_fn_type_params(ts_path, type_params),
                return_type: Box::new(
                    return_type
                        .as_ref()
                        .map(|t| self.process_type(ts_path, &t.type_ann))
                        .unwrap_or(TypeInfo::PrimitiveAny {}),
                ),
            }),
        }
    }

    fn process_decl(&mut self, ts_path: &Path, decl: &Decl) -> Vec<Type> {
        match decl {
            Decl::TsInterface(iface) => vec![self.process_ts_interface(ts_path, iface)],
            Decl::TsEnum(enm) => vec![self.process_ts_enum(ts_path, enm)],
            Decl::TsTypeAlias(alias) => vec![self.process_ts_alias(ts_path, alias)],
            Decl::Class(class) => vec![self.process_class_type(ts_path, class)],
            Decl::Var(VarDecl { decls, .. }) => decls
                .iter()
                .map(|var| self.process_var(ts_path, var))
                .collect(),
            Decl::TsModule(TsModuleDecl { id, body, .. }) => {
                let name = match id {
                    TsModuleName::Ident(ident) => ident.sym.to_string(),
                    TsModuleName::Str(s) => s.value.to_string(),
                };

                let full_ns = {
                    let mut full_ns = self
                        .namespace_stack
                        .last()
                        .unwrap_or(&Default::default())
                        .clone();
                    full_ns.push(name);
                    full_ns
                };
                self.namespace_stack.push(full_ns);

                for b in body {
                    match b {
                        TsNamespaceBody::TsModuleBlock(block) => {
                            self.process_module_items(ts_path, &block.body)
                        }
                        TsNamespaceBody::TsNamespaceDecl(_) => {
                            panic!("what is an inner namespace decl?")
                        }
                    }
                }

                self.namespace_stack.pop();

                Default::default()
            }
            Decl::Fn(fn_decl) => vec![self.process_fn_decl(ts_path, fn_decl)],
        }
    }

    fn process_export_decl(&mut self, ts_path: &Path, ExportDecl { decl, .. }: &ExportDecl) {
        let types = self.process_decl(ts_path, decl);

        types
            .into_iter()
            .map(|mut typ| {
                typ.is_exported = true;
                typ
            })
            .for_each(|typ| {
                let type_name = typ.name.to_name().to_string();

                self.set_type_for_name_for_file(ts_path, TypeIdent::Name(type_name), typ);
            });
    }

    fn process_named_export(
        &mut self,
        ts_path: &Path,
        NamedExport {
            src, specifiers, ..
        }: &NamedExport,
    ) {
        if src.is_none() && specifiers.is_empty() {
            return;
        }

        let src = src.as_ref().expect("need a src").value.to_string();
        let dir = ts_path.parent().expect("All files must have a parent");
        let file = self
            .process_module(
                Some(dir.to_path_buf()),
                &src
            )
            .expect("failed to process module");

        let to_export = specifiers
            .iter()
            .map(|spec| match spec {
                ExportSpecifier::Named(ExportNamedSpecifier { orig, exported, .. }) => {
                    Type {
                        name: TypeName::for_name(ts_path, &exported.as_ref().unwrap_or(orig).sym.to_string()),
                        is_exported: true,
                        info: TypeInfo::NamespaceImport(NamespaceImport::Named {
                            src: file.to_path_buf(),
                            name: orig.sym.to_string(),
                        })
                    }
                },
                ExportSpecifier::Default(ExportDefaultSpecifier { exported }) => {
                    Type {
                        name: TypeName::for_name(ts_path, &exported.sym.to_string()),
                        is_exported: true,
                        info: TypeInfo::NamespaceImport(NamespaceImport::Default {
                            src: file.to_path_buf(),
                        })
                    }
                },
                ExportSpecifier::Namespace(ExportNamespaceSpecifier { name, .. }) => {
                    Type {
                        name: TypeName::for_name(ts_path, &name.sym.to_string()),
                        is_exported: true,
                        info: TypeInfo::NamespaceImport(NamespaceImport::All {
                            src: file.to_path_buf(),
                        })
                    }
                }
            }).for_each(|typ| {
                self.set_type_for_name_for_file(
                    ts_path,
                    typ.name.name.clone(),
                    typ,
                );
            });
    }

    fn process_module_decl(&mut self, ts_path: &Path, module_decl: &ModuleDecl) {
        match module_decl {
            ModuleDecl::Import(decl) => self.process_import_decl(&ts_path, &decl),
            ModuleDecl::ExportDecl(decl) => self.process_export_decl(&ts_path, &decl),
            ModuleDecl::ExportNamed(decl) => self.process_named_export(&ts_path, &decl),
            ModuleDecl::ExportDefaultDecl(_decl) => {
                println!("DEFAULT DECL, {:?}", _decl);
                ()
            }
            ModuleDecl::ExportDefaultExpr(_decl) => {
                println!("export default expr, {:?}", _decl);
                ()
            }
            ModuleDecl::ExportAll(decl) => self.process_export_all(&ts_path, &decl),
            ModuleDecl::TsImportEquals(_decl) => {
                println!("import equals, {:?}", _decl);
                ()
            }
            ModuleDecl::TsExportAssignment(_decl) => {
                println!("export assignment, {:?}", _decl);
                ()
            }
            ModuleDecl::TsNamespaceExport(_decl) => {
                println!("export namespace, {:?}", _decl);
                ()
            }
        }
    }

    fn process_stmt(&mut self, ts_path: &Path, stmt: &Stmt) {
        match stmt {
            Stmt::Decl(decl) => self
                .process_decl(ts_path, &decl)
                .into_iter()
                .for_each(|typ| {
                    let type_name = typ.name.to_name().to_string();

                    self.set_type_for_name_for_file(ts_path, TypeIdent::Name(type_name), typ);
                }),
            _ => (), // we don't deal with most statements
        }
    }
}
