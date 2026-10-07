//! Reject language extensibility and environmental effects instead of guessing purity.
use super::*;
use std::collections::BTreeMap;
use syn::{Expr, FnArg, Item, Pat, ReturnType, Stmt, Type, parse::Parser};

pub(super) fn check(
    library: &str,
    assertion: &str,
    case: &RustCase,
) -> Result<BTreeSet<String>, String> {
    let library = syn::parse_file(library).map_err(|e| format!("library syntax: {e}"))?;
    let test = syn::parse_file(assertion).map_err(|e| format!("assertion syntax: {e}"))?;
    if !library.attrs.is_empty() || !test.attrs.is_empty() || library.items.len() > 64 {
        return Err("unsupported crate attributes or function count".into());
    }
    let mut functions = BTreeMap::new();
    for item in &library.items {
        let Item::Fn(function) = item else {
            return Err("library contains a non-function item".into());
        };
        if !function.attrs.is_empty()
            || functions
                .insert(function.sig.ident.to_string(), function)
                .is_some()
        {
            return Err("library function attributes or duplicate identity".into());
        }
    }
    let [Item::Fn(test)] = test.items.as_slice() else {
        return Err("detector must be one native test function".into());
    };
    if test.sig.ident != case.name
        || test.attrs.len() != 1
        || !test.attrs[0].path().is_ident("test")
        || !matches!(test.attrs[0].meta, syn::Meta::Path(_))
        || !test.sig.inputs.is_empty()
        || !matches!(test.sig.output, ReturnType::Default)
    {
        return Err("detector is not the exact unmodified native test".into());
    }
    let mut checker = Checker {
        functions: &functions,
        visiting: BTreeSet::new(),
        checked: BTreeSet::new(),
        fixtures: BTreeSet::new(),
        package: case.package.replace('-', "_"),
        nodes: 0,
    };
    for name in functions.keys() {
        checker.function(name)?;
    }
    checker.signature(test)?;
    let [statement] = test.block.stmts.as_slice() else {
        return Err("detector must execute exactly one unconditional assertion".into());
    };
    let mac = match statement {
        Stmt::Macro(value) if value.attrs.is_empty() => &value.mac,
        Stmt::Expr(Expr::Macro(value), _) if value.attrs.is_empty() => &value.mac,
        _ => return Err("detector is not a native assertion".into()),
    };
    if !mac.path.is_ident("assert_eq") {
        return Err("unsupported assertion macro".into());
    }
    let args = syn::punctuated::Punctuated::<Expr, syn::Token![,]>::parse_terminated
        .parse2(mac.tokens.clone())
        .map_err(|e| format!("assertion arguments: {e}"))?;
    if args.len() != 2 {
        return Err("assertion needs exactly two expressions and no custom message".into());
    }
    for arg in &args {
        checker.expression(arg, &BTreeSet::new(), true)?;
    }
    Ok(checker.fixtures)
}

struct Checker<'a> {
    functions: &'a BTreeMap<String, &'a syn::ItemFn>,
    visiting: BTreeSet<String>,
    checked: BTreeSet<String>,
    fixtures: BTreeSet<String>,
    package: String,
    nodes: usize,
}

impl Checker<'_> {
    fn signature(&self, function: &syn::ItemFn) -> Result<BTreeSet<String>, String> {
        let sig = &function.sig;
        if sig.asyncness.is_some()
            || sig.unsafety.is_some()
            || sig.abi.is_some()
            || sig.variadic.is_some()
            || !sig.generics.params.is_empty()
            || sig.generics.where_clause.is_some()
        {
            return Err("unsupported function signature".into());
        }
        let mut locals = BTreeSet::new();
        for arg in &sig.inputs {
            let FnArg::Typed(arg) = arg else {
                return Err("receiver is unsupported".into());
            };
            if !arg.attrs.is_empty() {
                return Err("argument attributes are unsupported".into());
            }
            primitive(&arg.ty)?;
            locals.insert(binding(&arg.pat)?);
        }
        if let ReturnType::Type(_, ty) = &sig.output {
            primitive(ty)?;
        }
        Ok(locals)
    }

    fn function(&mut self, name: &str) -> Result<(), String> {
        if self.checked.contains(name) {
            return Ok(());
        }
        if !self.visiting.insert(name.into()) {
            return Err("recursive input dependency".into());
        }
        let function = self
            .functions
            .get(name)
            .ok_or_else(|| format!("unresolved function {name}"))?;
        let locals = self.signature(function)?;
        self.block(&function.block, &locals, false)?;
        self.visiting.remove(name);
        self.checked.insert(name.into());
        Ok(())
    }

    fn block(
        &mut self,
        block: &syn::Block,
        locals: &BTreeSet<String>,
        test: bool,
    ) -> Result<(), String> {
        let mut locals = locals.clone();
        for statement in &block.stmts {
            match statement {
                Stmt::Local(local) if local.attrs.is_empty() => {
                    let name = binding(&local.pat)?;
                    let init = local.init.as_ref().ok_or("uninitialized local")?;
                    if init.diverge.is_some() {
                        return Err("let-else is unsupported".into());
                    }
                    self.expression(&init.expr, &locals, test)?;
                    locals.insert(name);
                }
                Stmt::Expr(expr, _) => self.expression(expr, &locals, test)?,
                _ => return Err("unsupported block item or macro".into()),
            }
        }
        Ok(())
    }

    fn expression(
        &mut self,
        expr: &Expr,
        locals: &BTreeSet<String>,
        test: bool,
    ) -> Result<(), String> {
        self.nodes += 1;
        if self.nodes > 1024 {
            return Err("closed input expression allowance exceeded".into());
        }
        match expr {
            Expr::Lit(v)
                if v.attrs.is_empty()
                    && matches!(
                        v.lit,
                        syn::Lit::Int(_) | syn::Lit::Bool(_) | syn::Lit::Str(_) | syn::Lit::Char(_)
                    ) =>
            {
                Ok(())
            }
            Expr::Path(v)
                if v.attrs.is_empty()
                    && v.qself.is_none()
                    && names(&v.path).is_some_and(|p| p.len() == 1 && locals.contains(&p[0])) =>
            {
                Ok(())
            }
            Expr::Paren(v) if v.attrs.is_empty() => self.expression(&v.expr, locals, test),
            Expr::Group(v) if v.attrs.is_empty() => self.expression(&v.expr, locals, test),
            Expr::Unary(v)
                if v.attrs.is_empty() && matches!(v.op, syn::UnOp::Neg(_) | syn::UnOp::Not(_)) =>
            {
                self.expression(&v.expr, locals, test)
            }
            Expr::Binary(v)
                if v.attrs.is_empty()
                    && matches!(
                        v.op,
                        syn::BinOp::Add(_)
                            | syn::BinOp::Sub(_)
                            | syn::BinOp::Mul(_)
                            | syn::BinOp::Div(_)
                            | syn::BinOp::Rem(_)
                            | syn::BinOp::And(_)
                            | syn::BinOp::Or(_)
                            | syn::BinOp::BitXor(_)
                            | syn::BinOp::BitAnd(_)
                            | syn::BinOp::BitOr(_)
                            | syn::BinOp::Shl(_)
                            | syn::BinOp::Shr(_)
                            | syn::BinOp::Eq(_)
                            | syn::BinOp::Lt(_)
                            | syn::BinOp::Le(_)
                            | syn::BinOp::Ne(_)
                            | syn::BinOp::Ge(_)
                            | syn::BinOp::Gt(_)
                    ) =>
            {
                self.expression(&v.left, locals, test)?;
                self.expression(&v.right, locals, test)
            }
            Expr::Block(v) if v.attrs.is_empty() && v.label.is_none() => {
                self.block(&v.block, locals, test)
            }
            Expr::If(v) if v.attrs.is_empty() => {
                self.expression(&v.cond, locals, test)?;
                self.block(&v.then_branch, locals, test)?;
                if let Some((_, other)) = &v.else_branch {
                    self.expression(other, locals, test)?;
                }
                Ok(())
            }
            Expr::Return(v) if v.attrs.is_empty() && !test => {
                if let Some(expr) = &v.expr {
                    self.expression(expr, locals, test)?;
                }
                Ok(())
            }
            Expr::Call(v) if v.attrs.is_empty() => {
                let Expr::Path(path) = v.func.as_ref() else {
                    return Err("indirect function call".into());
                };
                if path.qself.is_some() || !path.attrs.is_empty() {
                    return Err("qualified function call".into());
                }
                let parts = names(&path.path).ok_or("generic or absolute function call")?;
                let function = match parts.as_slice() {
                    [name] if !test => name,
                    [root, name] if root == if test { &self.package } else { "crate" } => name,
                    _ => return Err("function is outside the closed library".into()),
                };
                for arg in &v.args {
                    self.expression(arg, locals, test)?;
                }
                self.function(function)
            }
            Expr::Macro(v) if v.attrs.is_empty() && test && v.mac.path.is_ident("include_str") => {
                let path: syn::LitStr = syn::parse2(v.mac.tokens.clone())
                    .map_err(|e| format!("literal fixture path: {e}"))?;
                let path = path.value();
                let name = path
                    .strip_prefix("../fixtures/")
                    .ok_or("fixture must be below fixtures/")?;
                if name.is_empty()
                    || !name.split('/').all(|p| {
                        !p.is_empty()
                            && p != "."
                            && p != ".."
                            && p.chars()
                                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
                    })
                {
                    return Err("unsafe literal fixture path".into());
                }
                self.fixtures.insert(format!("fixtures/{name}"));
                Ok(())
            }
            _ => Err("unsupported Rust expression or hidden execution input".into()),
        }
    }
}

fn names(path: &syn::Path) -> Option<Vec<String>> {
    (path.leading_colon.is_none()
        && path
            .segments
            .iter()
            .all(|s| matches!(s.arguments, syn::PathArguments::None)))
    .then(|| path.segments.iter().map(|s| s.ident.to_string()).collect())
}

fn binding(pat: &Pat) -> Result<String, String> {
    match pat {
        Pat::Ident(v)
            if v.attrs.is_empty()
                && v.by_ref.is_none()
                && v.subpat.is_none()
                && v.mutability.is_none() =>
        {
            Ok(v.ident.to_string())
        }
        Pat::Type(v) if v.attrs.is_empty() => {
            primitive(&v.ty)?;
            binding(&v.pat)
        }
        _ => Err("unsupported argument or local binding".into()),
    }
}

fn primitive(ty: &Type) -> Result<(), String> {
    match ty {
        Type::Path(v)
            if v.qself.is_none()
                && names(&v.path).is_some_and(|p| {
                    p.len() == 1
                        && matches!(
                            p[0].as_str(),
                            "bool"
                                | "char"
                                | "u8"
                                | "u16"
                                | "u32"
                                | "u64"
                                | "u128"
                                | "usize"
                                | "i8"
                                | "i16"
                                | "i32"
                                | "i64"
                                | "i128"
                                | "isize"
                        )
                }) =>
        {
            Ok(())
        }
        Type::Reference(v)
            if v.mutability.is_none()
                && v.lifetime.as_ref().is_none_or(|l| l.ident == "static")
                && matches!(v.elem.as_ref(), Type::Path(p) if p.qself.is_none() && p.path.is_ident("str")) =>
        {
            Ok(())
        }
        Type::Tuple(v) if v.elems.is_empty() => Ok(()),
        _ => Err("non-primitive or extensible Rust type".into()),
    }
}
