use crate::ast::*;
use crate::diagnostic::RillError;
use crate::types::Value;
use std::collections::BTreeMap;
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Type {
    Unknown,
    Null,
    Bool,
    Int,
    Float,
    Str,
    Bytes,
    List(Box<Type>),
    Tuple(Vec<Type>),
    Map,
    Duration,
    Path,
    Regex,
    Error,
    Option(Box<Type>),
    Result(Box<Type>, Box<Type>),
    ProcessResult,
    ProcessHandle,
    Stream,
    Function { arity: Option<usize>, params: Option<Vec<Type>>, return_type: Box<Type> },
}

impl Type {
    fn is_unknown(&self) -> bool {
        matches!(self, Type::Unknown)
    }

    fn is_numeric(&self) -> bool {
        matches!(self, Type::Int | Type::Float | Type::Unknown)
    }

    fn list_item(&self) -> Type {
        match self {
            Type::List(inner) => *inner.clone(),
            Type::Unknown => Type::Unknown,
            _ => Type::Unknown,
        }
    }
}

impl fmt::Display for Type {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Type::Unknown => write!(f, "Unknown"),
            Type::Null => write!(f, "Null"),
            Type::Bool => write!(f, "Bool"),
            Type::Int => write!(f, "Int"),
            Type::Float => write!(f, "Float"),
            Type::Str => write!(f, "Str"),
            Type::Bytes => write!(f, "Bytes"),
            Type::List(inner) => write!(f, "List[{inner}]"),
            Type::Tuple(elements) => {
                write!(f, "Tuple[")?;
                for (index, element) in elements.iter().enumerate() {
                    if index > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{element}")?;
                }
                write!(f, "]")
            }
            Type::Map => write!(f, "Map"),
            Type::Duration => write!(f, "Duration"),
            Type::Path => write!(f, "Path"),
            Type::Regex => write!(f, "Regex"),
            Type::Error => write!(f, "Error"),
            Type::Option(inner) => write!(f, "Option[{inner}]"),
            Type::Result(ok, err) => write!(f, "Result[{ok}, {err}]"),
            Type::ProcessResult => write!(f, "ProcessResult"),
            Type::ProcessHandle => write!(f, "ProcessHandle"),
            Type::Stream => write!(f, "Stream"),
            Type::Function { .. } => write!(f, "Fn"),
        }
    }
}

#[derive(Debug, Clone)]
struct TypeBinding {
    ty: Type,
    mutable: bool,
}

#[derive(Debug, Clone)]
struct Env {
    scopes: Vec<BTreeMap<String, TypeBinding>>,
}

impl Env {
    fn new() -> Self {
        let mut root = BTreeMap::new();
        let insert = |root: &mut BTreeMap<String, TypeBinding>, name: &str, ty: Type| {
            root.insert(name.to_string(), TypeBinding { ty, mutable: true });
        };
        insert(&mut root, "args", Type::List(Box::new(Type::Str)));
        insert(&mut root, "env", Type::Map);
        insert(&mut root, "input", Type::Map);
        for builtin in crate::methods::BUILTINS {
            insert(
                &mut root,
                builtin.name,
                Type::Function { arity: None, params: None, return_type: Box::new(Type::Unknown) },
            );
        }
        Self { scopes: vec![root] }
    }

    fn push(&mut self) {
        self.scopes.push(BTreeMap::new());
    }

    fn pop(&mut self) {
        self.scopes.pop();
    }

    fn set(&mut self, name: String, ty: Type) {
        self.scopes
            .last_mut()
            .expect("type env always has root scope")
            .insert(name, TypeBinding { ty, mutable: true });
    }

    fn bind(&mut self, name: String, ty: Type, mutable: bool) {
        self.scopes
            .last_mut()
            .expect("type env always has root scope")
            .insert(name, TypeBinding { ty, mutable });
    }

    fn get(&self, name: &str) -> Option<Type> {
        self.scopes.iter().rev().find_map(|scope| scope.get(name).map(|b| b.ty.clone()))
    }

    fn is_mutable(&self, name: &str) -> Option<bool> {
        self.scopes.iter().rev().find_map(|scope| scope.get(name).map(|b| b.mutable))
    }
}

pub fn check(program: &Program) -> Result<(), RillError> {
    check_module(program, &[]).map(|_| ())
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ModuleExports {
    symbols: BTreeMap<String, Type>,
}

pub(crate) fn check_module(
    program: &Program,
    imports: &[&ModuleExports],
) -> Result<ModuleExports, RillError> {
    let mut env = Env::new();
    for import in imports {
        for (name, ty) in &import.symbols {
            env.set(name.clone(), ty.clone());
        }
    }
    let mut checker =
        Checker { env, return_expected: None, loop_depth: 0, for_body_depth: 0, function_depth: 0 };
    checker.check_program(program)
}

struct Checker {
    env: Env,
    return_expected: Option<Type>,
    loop_depth: usize,
    for_body_depth: usize,
    function_depth: usize,
}

impl Checker {
    fn bind_pattern(
        &mut self,
        pattern: &LetPattern,
        ty: Type,
        mutable: bool,
    ) -> Result<(), RillError> {
        match pattern {
            LetPattern::Ident(name) => self.env.bind(name.clone(), ty, mutable),
            LetPattern::List(names) => {
                for name in names {
                    if name != "_" {
                        self.env.bind(name.clone(), Type::Unknown, mutable);
                    }
                }
            }
            LetPattern::Tuple(names) => {
                let elements = match ty {
                    Type::Tuple(elements) => elements,
                    Type::Unknown => vec![Type::Unknown; names.len()],
                    other => {
                        return Err(RillError::new(format!(
                            "tuple destructuring requires a Tuple, got {other}"
                        )))
                    }
                };
                if elements.len() != names.len() {
                    return Err(RillError::new(format!(
                        "tuple destructuring needs {} bindings, got {}",
                        elements.len(),
                        names.len()
                    )));
                }
                for (name, element) in names.iter().zip(elements) {
                    if name != "_" {
                        self.env.bind(name.clone(), element, mutable);
                    }
                }
            }
            LetPattern::Map(pairs) => {
                for (key, alias) in pairs {
                    let binding = alias.as_deref().unwrap_or(key.as_str());
                    self.env.bind(binding.to_string(), Type::Unknown, mutable);
                }
            }
        }
        Ok(())
    }

    fn check_program(&mut self, program: &Program) -> Result<ModuleExports, RillError> {
        for stmt in &program.stmts {
            if let Stmt::Fn { name, function } = stmt {
                self.env.set(
                    name.clone(),
                    Type::Function {
                        arity: Some(function.params.len()),
                        params: Some(
                            function
                                .params
                                .iter()
                                .map(|p| {
                                    p.type_name
                                        .as_ref()
                                        .map(type_from_name)
                                        .unwrap_or(Type::Unknown)
                                })
                                .collect(),
                        ),
                        return_type: Box::new(Type::Unknown),
                    },
                );
            }
        }
        self.check_stmts(&program.stmts)?;
        let mut symbols = BTreeMap::new();
        for name in top_level_names(&program.stmts) {
            if let Some(ty) = self.env.get(&name) {
                symbols.insert(name, ty);
            }
        }
        Ok(ModuleExports { symbols })
    }

    fn check_stmts(&mut self, stmts: &[Stmt]) -> Result<Type, RillError> {
        let mut last = Type::Null;
        for stmt in stmts {
            last = self.check_stmt(stmt)?;
        }
        Ok(last)
    }

    fn check_stmt(&mut self, stmt: &Stmt) -> Result<Type, RillError> {
        match stmt {
            Stmt::Use(module) => {
                if !matches!(
                    module.as_str(),
                    "json"
                        | "toml"
                        | "yaml"
                        | "csv"
                        | "url"
                        | "path"
                        | "fs"
                        | "process"
                        | "time"
                        | "regex"
                        | "archive"
                        | "hashes"
                ) && !module.starts_with("./")
                    && !module.starts_with("../")
                {
                    return Err(RillError::new(format!("unknown module '{module}'")));
                }
                Ok(Type::Null)
            }
            Stmt::Let { pattern, type_name, expr, mutable } => {
                let inferred = self.check_expr(expr)?;
                let ty = if let Some(type_name) = type_name {
                    let annotated = type_from_name(type_name);
                    let span = expr.span();
                    ensure_assignable(&annotated, &inferred).map_err(|e| match span {
                        Some(s) => e.with_span(s),
                        None => e,
                    })?;
                    annotated
                } else {
                    inferred
                };
                self.bind_pattern(pattern, ty, *mutable)?;
                Ok(Type::Null)
            }
            Stmt::AssignEnv { name: _, expr } => {
                let ty = self.check_expr(expr)?;
                ensure_assignable(&Type::Str, &ty)?;
                Ok(Type::Null)
            }
            Stmt::Assign { name, expr, .. } => {
                if matches!(self.env.is_mutable(name), Some(false)) {
                    return Err(RillError::new(format!(
                        "cannot assign to immutable variable '{name}' (declared with 'let'; use 'var' for mutable variables)"
                    )));
                }
                self.check_expr(expr)?;
                Ok(Type::Null)
            }
            Stmt::IndexAssign { name, index, op, expr } => {
                if matches!(self.env.is_mutable(name), Some(false)) {
                    return Err(RillError::new(format!(
                        "cannot assign to immutable variable '{name}' (declared with 'let'; use 'var' for mutable variables)"
                    )));
                }
                let collection = self
                    .env
                    .get(name)
                    .ok_or_else(|| RillError::new(format!("unknown identifier '{name}'")))?;
                let index_ty = self.check_expr(index)?;
                let value_ty = self.check_expr(expr)?;
                if let Type::Tuple(elements) = collection {
                    ensure_assignable(&Type::Int, &index_ty)?;
                    let raw_index = constant_int(index).ok_or_else(|| {
                        RillError::new("tuple assignment index must be an integer literal")
                    })?;
                    let position = normalize_tuple_index(raw_index, elements.len())?;
                    let target = &elements[position];
                    let assigned = compound_assignment_type(*op, target, &value_ty)?;
                    ensure_assignable(target, &assigned)?;
                }
                Ok(Type::Null)
            }
            Stmt::Args(group) => {
                for field in &group.fields {
                    if let Some(default) = &field.default {
                        ensure_assignable(
                            &type_from_name(&field.type_name),
                            &self.check_expr(default)?,
                        )?;
                    }
                }
                for child in &group.subcommands {
                    self.check_arg_group(child)?;
                }
                self.env.set("args".to_string(), Type::Map);
                Ok(Type::Null)
            }
            Stmt::InputLines => Ok(Type::Null),
            Stmt::Fn { name, function } => {
                self.env.set(
                    name.clone(),
                    Type::Function {
                        arity: Some(function.params.len()),
                        params: Some(
                            function
                                .params
                                .iter()
                                .map(|p| {
                                    p.type_name
                                        .as_ref()
                                        .map(type_from_name)
                                        .unwrap_or(Type::Unknown)
                                })
                                .collect(),
                        ),
                        return_type: Box::new(Type::Unknown),
                    },
                );
                self.env.push();
                for param in &function.params {
                    self.env.set(
                        param.name.clone(),
                        param.type_name.as_ref().map(type_from_name).unwrap_or(Type::Unknown),
                    );
                }
                let previous_return = self.return_expected.take();
                let previous_loop = std::mem::replace(&mut self.loop_depth, 0);
                self.function_depth += 1;
                self.return_expected = function.return_type.as_ref().map(type_from_name);
                let body_ty = self.check_stmts(&function.body)?;
                if let Some(return_type) = &function.return_type {
                    let last_span = function.body.last().and_then(|s| match s {
                        Stmt::Return(Some(e)) | Stmt::Expr(e) | Stmt::Emit(e) => e.span(),
                        Stmt::Let { expr, .. } => expr.span(),
                        _ => None,
                    });
                    ensure_function_return(&type_from_name(return_type), &body_ty).map_err(
                        |e| match last_span {
                            Some(s) => e.with_span(s),
                            None => e,
                        },
                    )?;
                }
                self.return_expected = previous_return;
                self.loop_depth = previous_loop;
                self.function_depth -= 1;
                self.env.pop();
                self.env.set(
                    name.clone(),
                    Type::Function {
                        arity: Some(function.params.len()),
                        params: Some(
                            function
                                .params
                                .iter()
                                .map(|p| {
                                    p.type_name
                                        .as_ref()
                                        .map(type_from_name)
                                        .unwrap_or(Type::Unknown)
                                })
                                .collect(),
                        ),
                        return_type: Box::new(
                            function.return_type.as_ref().map(type_from_name).unwrap_or(body_ty),
                        ),
                    },
                );
                Ok(Type::Null)
            }
            Stmt::Emit(expr) => {
                if self.for_body_depth == 0 {
                    return Err(RillError::new(
                        "'emit' is only valid inside a for expression body",
                    ));
                }
                self.check_expr(expr)?;
                Ok(Type::Null)
            }
            Stmt::If { cond, pattern, then_body, else_body } => {
                let cond_ty = self.check_expr(cond)?;
                if pattern.is_none() && !matches!(cond_ty, Type::Bool | Type::Unknown) {
                    return Err(RillError::new(format!(
                        "if condition must be Bool, got {cond_ty}"
                    )));
                }
                self.env.push();
                if let Some(pattern) = pattern {
                    self.bind_pattern(pattern, cond_ty.clone(), true)?;
                }
                if let Expr::RegexMatch { pattern, .. } = cond.unspanned() {
                    for name in regex_capture_names(pattern)? {
                        self.env.set(name, Type::Str);
                    }
                }
                let then_ty = self.check_stmts(then_body)?;
                self.env.pop();
                self.env.push();
                let else_ty = self.check_stmts(else_body)?;
                self.env.pop();
                Ok(common_type(then_ty, else_ty))
            }
            Stmt::WithEnv { bindings, body } => {
                for (_, expr) in bindings {
                    let ty = self.check_expr(expr)?;
                    ensure_assignable(&Type::Str, &ty)?;
                }
                self.env.push();
                self.check_stmts(body)?;
                self.env.pop();
                Ok(Type::Null)
            }
            Stmt::On { host, body } => {
                let host_ty = self.check_expr(host)?;
                if !matches!(host_ty, Type::Str | Type::List(_) | Type::Unknown) {
                    return Err(RillError::new(format!("on needs Str or List, got {host_ty}")));
                }
                self.env.push();
                self.check_stmts(body)?;
                self.env.pop();
                Ok(Type::Null)
            }
            Stmt::Return(expr) => {
                if self.function_depth == 0 {
                    return Err(RillError::new("return outside function"));
                }
                let (value_ty, span) = match expr {
                    Some(expr) => (self.check_expr(expr)?, expr.span()),
                    None => (Type::Null, None),
                };
                if let Some(expected) = &self.return_expected {
                    ensure_function_return(expected, &value_ty).map_err(|e| match span {
                        Some(s) => e.with_span(s),
                        None => e,
                    })?;
                }
                Ok(value_ty)
            }
            Stmt::Continue | Stmt::Break => {
                if self.loop_depth == 0 {
                    return Err(RillError::new("break/continue outside loop"));
                }
                Ok(Type::Null)
            }
            Stmt::While { cond, body } => {
                self.check_expr(cond)?;
                self.loop_depth += 1;
                self.check_stmts(body)?;
                self.loop_depth -= 1;
                Ok(Type::Null)
            }
            Stmt::Expr(expr) => self.check_expr(expr),
        }
    }

    fn check_arg_group(&mut self, group: &ArgGroup) -> Result<(), RillError> {
        for field in &group.fields {
            if let Some(default) = &field.default {
                ensure_assignable(&type_from_name(&field.type_name), &self.check_expr(default)?)?;
            }
        }
        for child in &group.subcommands {
            self.check_arg_group(child)?;
        }
        Ok(())
    }

    fn check_expr(&mut self, expr: &Expr) -> Result<Type, RillError> {
        match expr {
            Expr::Located { expr, span } => {
                self.check_expr(expr).map_err(|error| error.with_span(*span))
            }
            Expr::Literal(value) => Ok(type_from_literal(value)),
            Expr::Ident(name) => {
                // none/ok/some/err used bare (no call parens) should resolve to their
                // natural types, not the Type::Function they're stored as in the env.
                // none bare = None (Option[Unknown]); ok/some/err bare = unknown, treat
                // as passthrough to avoid false "got Fn" errors.
                if name == "none" {
                    return Ok(Type::Option(Box::new(Type::Unknown)));
                }
                self.env
                    .get(name)
                    .ok_or_else(|| RillError::new(format!("unknown identifier '{name}'")))
            }
            Expr::List(items) => {
                let mut item_ty = Type::Unknown;
                for item in items {
                    item_ty = common_type(item_ty, self.check_expr(item)?);
                }
                Ok(Type::List(Box::new(item_ty)))
            }
            Expr::Tuple(items) => items
                .iter()
                .map(|item| self.check_expr(item))
                .collect::<Result<Vec<_>, _>>()
                .map(Type::Tuple),
            Expr::Map(pairs) => {
                for (_, expr) in pairs {
                    self.check_expr(expr)?;
                }
                Ok(Type::Map)
            }
            Expr::Call { callee, args } => self.check_call(callee, args),
            Expr::Index { base, index } => {
                let base_ty = self.check_expr(base)?;
                let index_ty = self.check_expr(index)?;
                if let Type::Tuple(elements) = base_ty {
                    ensure_assignable(&Type::Int, &index_ty)?;
                    if let Some(index) = constant_int(index) {
                        return normalize_tuple_index(index, elements.len())
                            .map(|position| elements[position].clone());
                    }
                    let mut elements = elements.into_iter();
                    let first = elements.next().unwrap_or(Type::Unknown);
                    return Ok(elements.fold(first, common_type));
                }
                Ok(Type::Unknown)
            }
            Expr::Field { base, name } => self.check_field(base, name),
            Expr::Binary { op, left, right } => self.check_binary(*op, left, right),
            Expr::Pipe { kind, left, right } => self.check_pipe(*kind, left, right),
            Expr::Command(command) => {
                self.check_command(command)?;
                Ok(if command.check_exit { Type::Str } else { Type::ProcessResult })
            }
            Expr::RegexMatch { value, .. } => {
                let ty = self.check_expr(value)?;
                if !matches!(ty, Type::Str | Type::Unknown) {
                    return Err(RillError::new(format!("regex match needs Str, got {ty}")));
                }
                if let Expr::RegexMatch { pattern, .. } = expr {
                    compile_regex(pattern)?;
                }
                Ok(Type::Bool)
            }
            Expr::RegexSubstitution { value, pattern, .. } => {
                let ty = self.check_expr(value)?;
                if !matches!(ty, Type::Str | Type::Unknown) {
                    return Err(RillError::new(format!("regex substitution needs Str, got {ty}")));
                }
                compile_regex(pattern)?;
                Ok(Type::Str)
            }
            Expr::Match { value, arms } => self.check_match(value, arms),
            Expr::NamedArg { value, .. } => self.check_expr(value),
            Expr::Try(inner) => match self.check_expr(inner)? {
                Type::Option(value) => Ok(*value),
                Type::Result(value, _) => Ok(*value),
                Type::Unknown => Ok(Type::Unknown),
                other => Err(RillError::new(format!("? needs Option or Result, got {other}"))),
            },
            Expr::For { mode, workers, pattern, iter, body } => {
                if let Some(workers) = workers {
                    let workers_ty = self.check_expr(workers)?;
                    if workers_ty != Type::Int && workers_ty != Type::Unknown {
                        return Err(RillError::new(format!(
                            "parallel worker count needs Int, got {workers_ty}"
                        )));
                    }
                }
                let iter_ty = self.check_expr(iter)?;
                if !matches!(iter_ty, Type::List(_) | Type::Stream | Type::Unknown) {
                    return Err(RillError::new(format!(
                        "for loop needs List or Stream, got {iter_ty}"
                    )));
                }
                self.env.push();
                let item_ty =
                    if matches!(iter_ty, Type::Stream) { Type::Map } else { iter_ty.list_item() };
                self.bind_pattern(pattern, item_ty, true)?;
                self.loop_depth += 1;
                self.for_body_depth += 1;
                self.check_stmts(body)?;
                self.for_body_depth -= 1;
                self.loop_depth -= 1;
                self.env.pop();
                Ok(match mode {
                    ForMode::Sequential | ForMode::Parallel => Type::List(Box::new(Type::Unknown)),
                    ForMode::Race => Type::Unknown,
                })
            }
            Expr::Block(stmts) => {
                self.env.push();
                let ty = self.check_stmts(stmts)?;
                self.env.pop();
                Ok(ty)
            }
            Expr::Closure { params, body } => {
                let previous_loop = std::mem::replace(&mut self.loop_depth, 0);
                let previous_return = self.return_expected.take();
                self.return_expected = Some(Type::Unknown);
                self.function_depth += 1;
                self.env.push();
                for param in params {
                    self.env.set(param.clone(), Type::Unknown);
                }
                let return_type = self.check_stmts(body)?;
                self.loop_depth = previous_loop;
                self.return_expected = previous_return;
                self.function_depth -= 1;
                self.env.pop();
                Ok(Type::Function {
                    arity: Some(params.len()),
                    params: None,
                    return_type: Box::new(return_type),
                })
            }
            Expr::Unary { op: UnaryOp::Not, operand } => {
                let ty = self.check_expr(operand)?;
                if !matches!(ty, Type::Bool | Type::Unknown) {
                    return Err(RillError::new(format!("not requires Bool, got {ty}")));
                }
                Ok(Type::Bool)
            }
            Expr::Implicit => Ok(self.env.get("it").unwrap_or(Type::Unknown)),
            Expr::ImplicitField(_) => Ok(Type::Unknown),
        }
    }

    fn check_call(&mut self, callee: &Expr, args: &[Expr]) -> Result<Type, RillError> {
        if let Expr::Ident(name) = callee.unspanned() {
            if let Some(method) = crate::methods::moved_function(name) {
                if let Some(function) = self.env.get(name) {
                    let types = args
                        .iter()
                        .map(|arg| self.check_expr(arg))
                        .collect::<Result<Vec<_>, _>>()?;
                    return check_typed_call(&function, &types);
                } else {
                    return Err(RillError::new(format!(
                        "{name}(...) was removed; use receiver.{method}(...)"
                    )));
                }
            }
        }
        if let Expr::ImplicitField(name) = callee.unspanned() {
            return self.check_call(
                &Expr::Field { base: Box::new(Expr::Implicit), name: name.clone() },
                args,
            );
        }
        if let Expr::Field { base, name } = callee.unspanned() {
            if let Expr::Ident(module) = base.unspanned() {
                if crate::methods::is_namespace(module)
                    && (self.env.get(module).is_none()
                        || (module == "path"
                            && matches!(
                                self.env.get(module),
                                Some(Type::Function { arity: None, .. })
                            )))
                {
                    // Stream.tail handled specially — not in METHODS table
                    if module == "Stream" && name == "tail" {
                        for arg in args {
                            self.check_expr(arg)?;
                        }
                        return Ok(Type::Stream);
                    }
                    if let Some(call) = crate::methods::namespace_method(module, name) {
                        return self.check_namespace_method(module, name, call, args);
                    }
                    return Err(RillError::new(format!("{module} has no function '{name}'")));
                }
            }
            if crate::methods::lookup(name).is_some() {
                return self.check_method(base, name, args);
            }
        }
        self.check_function_call(callee, args)
    }

    fn check_namespace_method(
        &mut self,
        module: &str,
        name: &str,
        call: crate::methods::NamespaceCall,
        args: &[Expr],
    ) -> Result<Type, RillError> {
        let signature = call.method;
        let expected = (signature.min_args + 1)..=(signature.max_args + 1);
        if !expected.contains(&args.len()) {
            return Err(RillError::new(format!(
                "{module}.{name} needs {}{} args, got {}",
                signature.min_args + 1,
                if signature.min_args == signature.max_args {
                    String::new()
                } else {
                    format!(" to {}", signature.max_args + 1)
                },
                args.len()
            )));
        }
        let mut types =
            args.iter().map(|arg| self.check_expr(arg)).collect::<Result<Vec<_>, _>>()?;
        match call.adapter {
            crate::methods::ReceiverAdapter::Direct => {}
            crate::methods::ReceiverAdapter::Path => {
                if !matches!(types[0], Type::Str | Type::Path | Type::Unknown) {
                    return Err(RillError::new(format!(
                        "{module}.{name} needs a Str or Path, got {}",
                        types[0]
                    )));
                }
                types[0] = Type::Path;
            }
            crate::methods::ReceiverAdapter::Duration => {
                if !matches!(types[0], Type::Int | Type::Duration | Type::Unknown) {
                    return Err(RillError::new(format!(
                        "{module}.{name} needs Int seconds or Duration, got {}",
                        types[0]
                    )));
                }
                types[0] = Type::Duration;
            }
            crate::methods::ReceiverAdapter::SwapFirstTwo => types.swap(0, 1),
        }
        self.env.push();
        let bound = types
            .into_iter()
            .enumerate()
            .map(|(index, ty)| {
                let key = format!("$namespace_arg_{index}");
                self.env.set(key.clone(), ty);
                Expr::Ident(key)
            })
            .collect::<Vec<_>>();
        let result = self.check_method(&bound[0], signature.name, &bound[1..]);
        self.env.pop();
        result
    }

    fn check_method(&mut self, base: &Expr, name: &str, args: &[Expr]) -> Result<Type, RillError> {
        let receiver = self.check_expr(base)?;

        let kind = method_kind(&receiver);
        let method = crate::methods::lookup(name)
            .ok_or_else(|| RillError::new(format!("unknown method .{name}()")))?;
        method.validate(kind, args.len()).map_err(RillError::new)?;
        let mut arg_types = Vec::new();
        for (index, arg) in args.iter().enumerate() {
            match (arg.unspanned(), method.argument(index)) {
                (
                    Expr::NamedArg { name: label, .. },
                    crate::methods::Argument::NamedBool(expected),
                ) if label == expected => {}
                (Expr::NamedArg { name: label, .. }, _) => {
                    return Err(RillError::new(format!("unknown .{name}() argument '{label}'")));
                }
                (_, crate::methods::Argument::NamedBool(label)) => {
                    return Err(RillError::new(format!(
                        ".{name}() argument {} must be {label}=...",
                        index + 1
                    )));
                }
                _ => {}
            }
            let ty = if method.argument(index) == crate::methods::Argument::Function {
                self.env.push();
                self.env.set("it".into(), receiver.list_item());
                let ty = self.check_callback_expr(arg);
                self.env.pop();
                ty?
            } else {
                self.check_expr(arg)?
            };
            if !method.accepts_argument(kind, index, method_kind(&ty)) {
                return Err(arg.span().map_or_else(
                    || RillError::new(format!("invalid .{name}() argument {}: {ty}", index + 1)),
                    |s| {
                        RillError::spanned(
                            format!("invalid .{name}() argument {}: {ty}", index + 1),
                            s,
                        )
                    },
                ));
            }
            arg_types.push(ty);
        }
        if matches!(name, "run" | "spawn") {
            ensure_assignable(&Type::List(Box::new(Type::Str)), &receiver)?;
        }
        if name == "parallel" {
            ensure_assignable(&Type::List(Box::new(Type::List(Box::new(Type::Str)))), &receiver)?;
        }
        if method.arguments.contains(&crate::methods::Argument::Function) {
            self.check_collection_callback(name, &receiver, &arg_types[0])?;
        }
        Ok(resolve_method_return(method.returns, &receiver, &arg_types))
    }

    fn check_function_call(&mut self, callee: &Expr, args: &[Expr]) -> Result<Type, RillError> {
        if let Expr::Ident(name) = callee.unspanned() {
            return self.check_named_call(name, args);
        }
        let callee_ty = self.check_expr(callee)?;
        if callee_ty == Type::Unknown {
            for arg in args {
                self.check_expr(arg)?;
            }
            return Ok(Type::Unknown);
        }
        let Type::Function { arity, return_type, params } = callee_ty else {
            return Err(RillError::new(format!("{callee_ty} is not callable")));
        };
        if let Some(arity) = arity {
            ensure_arity("function", arity, args.len())?;
        }
        for (index, arg) in args.iter().enumerate() {
            let actual = self.check_expr(arg)?;
            if let Some(expected) = params.as_ref().and_then(|p| p.get(index)) {
                ensure_assignable(expected, &actual)?;
            }
        }
        Ok(*return_type)
    }

    fn check_named_call(&mut self, name: &str, args: &[Expr]) -> Result<Type, RillError> {
        if let Some(builtin) = crate::methods::lookup_builtin(name) {
            builtin.validate(args.len()).map_err(RillError::new)?;
            let mut arg_types = Vec::with_capacity(args.len());
            for (index, arg) in args.iter().enumerate() {
                let ty = self.check_expr(arg)?;
                if !builtin.argument(index).accepts(crate::methods::Kind::Unknown, method_kind(&ty))
                {
                    let expected = builtin.argument(index).expected_name().unwrap_or("value");
                    return Err(RillError::new(format!(
                        "type mismatch: expected {expected}, got {ty}"
                    )));
                }
                arg_types.push(ty);
            }
            return Ok(resolve_method_return(builtin.returns, &Type::Unknown, &arg_types));
        }
        let ty = self
            .env
            .get(name)
            .ok_or_else(|| RillError::new(format!("unknown function '{name}'")))?;
        let Type::Function { arity, params, return_type } = ty else {
            return Err(RillError::new(format!("{name} is not callable")));
        };
        if let Some(arity) = arity {
            ensure_arity(name, arity, args.len())?;
        }
        for (index, arg) in args.iter().enumerate() {
            let actual = self.check_expr(arg)?;
            if let Some(expected) = params.as_ref().and_then(|p| p.get(index)) {
                ensure_assignable(expected, &actual)?;
            }
        }
        Ok(*return_type)
    }

    fn check_match(&mut self, value: &Expr, arms: &[MatchArm]) -> Result<Type, RillError> {
        let value_ty = self.check_expr(value)?;
        let is_structural =
            matches!(value_ty, Type::Option(_) | Type::Result(_, _) | Type::Unknown);
        let is_str = matches!(value_ty, Type::Str | Type::Unknown);
        let mut result_ty = Type::Unknown;
        for arm in arms {
            self.env.push();
            match &arm.pattern {
                MatchPattern::Regex(pattern) => {
                    if !is_str {
                        return Err(RillError::new(format!(
                            "regex match arm requires Str value, got {value_ty}"
                        )));
                    }
                    for name in regex_capture_names(pattern)? {
                        self.env.set(name, Type::Str);
                    }
                }
                MatchPattern::Ok(binding) | MatchPattern::Err(binding) => {
                    if !is_structural {
                        return Err(RillError::new(format!(
                            "ok/err match arm requires Result value, got {value_ty}"
                        )));
                    }
                    if let Some(name) = binding {
                        self.env.set(name.clone(), Type::Unknown);
                    }
                }
                MatchPattern::Some(binding) => {
                    if !is_structural {
                        return Err(RillError::new(format!(
                            "some match arm requires Option value, got {value_ty}"
                        )));
                    }
                    if let Some(name) = binding {
                        self.env.set(name.clone(), Type::Unknown);
                    }
                }
                MatchPattern::None | MatchPattern::Wildcard => {}
            }
            result_ty = common_type(result_ty, self.check_expr(&arm.expr)?);
            self.env.pop();
        }
        Ok(result_ty)
    }

    fn check_field(&mut self, base: &Expr, name: &str) -> Result<Type, RillError> {
        if let Expr::Ident(module) = base.unspanned() {
            if crate::methods::is_namespace(module) && self.env.get(module).is_none() {
                if module == "Stream" && name == "tail" {
                    return Ok(Type::Function {
                        arity: None,
                        params: None,
                        return_type: Box::new(Type::Stream),
                    });
                }
                if crate::methods::namespace_method(module, name).is_some() {
                    return Ok(Type::Function {
                        arity: None,
                        params: None,
                        return_type: Box::new(Type::Unknown),
                    });
                }
                return Err(RillError::new(format!("{module} has no function '{name}'")));
            }
        }
        let base_ty = self.check_expr(base)?;
        match base_ty {
            Type::ProcessResult => match name {
                "status" => Ok(Type::Int),
                "out" | "err" => Ok(Type::Str),
                "ok" => Ok(Type::Bool),
                _ => Err(RillError::new(format!("ProcessResult has no field '{name}'"))),
            },
            Type::Map | Type::Unknown => Ok(Type::Unknown),
            ref ty
                if crate::methods::lookup(name)
                    .is_some_and(|method| method.accepts(method_kind(ty))) =>
            {
                Err(RillError::new(format!(".{name} is a method; call .{name}(...)")))
            }
            other => Err(RillError::new(format!("{other} has no field '{name}'"))),
        }
    }

    fn check_binary(&mut self, op: BinaryOp, left: &Expr, right: &Expr) -> Result<Type, RillError> {
        let left_ty = self.check_expr(left)?;
        let right_ty = self.check_expr(right)?;
        match op {
            BinaryOp::And | BinaryOp::Or => {
                if !matches!(left_ty, Type::Bool | Type::Unknown) {
                    return Err(RillError::new(format!(
                        "left side of {} requires Bool, got {left_ty}",
                        if op == BinaryOp::And { "&&" } else { "||" }
                    )));
                }
                if !matches!(right_ty, Type::Bool | Type::Unknown) {
                    return Err(RillError::new(format!(
                        "right side of {} requires Bool, got {right_ty}",
                        if op == BinaryOp::And { "&&" } else { "||" }
                    )));
                }
                Ok(Type::Bool)
            }
            BinaryOp::Eq | BinaryOp::Ne => Ok(Type::Bool),
            BinaryOp::Gt | BinaryOp::Ge | BinaryOp::Lt | BinaryOp::Le => {
                if (!left_ty.is_numeric() && !left_ty.is_unknown())
                    || (!right_ty.is_numeric() && !right_ty.is_unknown())
                {
                    return Err(RillError::new(format!(
                        "comparison needs numeric operands, got {left_ty} and {right_ty}"
                    )));
                }
                Ok(Type::Bool)
            }
            BinaryOp::Add if left_ty == Type::Str && right_ty == Type::Str => Ok(Type::Str),
            BinaryOp::Add if matches!((&left_ty, &right_ty), (Type::List(_), Type::List(_))) => {
                Ok(common_type(left_ty, right_ty))
            }
            BinaryOp::Add if left_ty == Type::Unknown || right_ty == Type::Unknown => {
                Ok(Type::Unknown)
            }
            BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div => {
                if !left_ty.is_numeric() || !right_ty.is_numeric() {
                    return Err(RillError::new(format!(
                        "numeric operator needs numeric operands, got {left_ty} and {right_ty}"
                    )));
                }
                Ok(if left_ty == Type::Unknown || right_ty == Type::Unknown {
                    Type::Unknown
                } else if left_ty == Type::Float || right_ty == Type::Float {
                    Type::Float
                } else {
                    Type::Int
                })
            }
            BinaryOp::Range => {
                if !left_ty.is_numeric() || !right_ty.is_numeric() {
                    return Err(RillError::new(format!(
                        "range requires numeric operands, got {left_ty} and {right_ty}"
                    )));
                }
                Ok(Type::List(Box::new(Type::Int)))
            }
        }
    }

    fn check_pipe(&mut self, kind: PipeKind, left: &Expr, right: &Expr) -> Result<Type, RillError> {
        match kind {
            PipeKind::Byte => {
                self.check_byte_pipe_left(left)?;
                match right.unspanned() {
                    Expr::Command(command) => {
                        self.check_command(command)?;
                        Ok(Type::ProcessResult)
                    }
                    expr if crate::methods::is_lines_consumer(expr) => {
                        Ok(Type::List(Box::new(Type::Str)))
                    }
                    _ => Err(RillError::new("right side of | must be a command or byte consumer")),
                }
            }
            PipeKind::Value => {
                let left_ty = self.check_expr(left)?;
                self.check_value_pipe_right(left_ty, right)
            }
        }
    }

    fn check_byte_pipe_left(&mut self, expr: &Expr) -> Result<(), RillError> {
        match expr.unspanned() {
            Expr::Command(command) => self.check_command(command),
            Expr::Pipe { kind: PipeKind::Byte, left, right } => {
                self.check_byte_pipe_left(left)?;
                match right.unspanned() {
                    Expr::Command(command) => self.check_command(command),
                    _ => Err(RillError::new("middle of byte pipeline must be a command")),
                }
            }
            _ => {
                self.check_expr(expr)?;
                Ok(())
            }
        }
    }

    fn check_value_pipe_right(&mut self, left_ty: Type, right: &Expr) -> Result<Type, RillError> {
        self.env.push();
        let result = if crate::methods::is_method_pipeline(right) {
            self.env.set("it".into(), left_ty);
            self.check_expr(right)
        } else {
            self.env.set("$pipe_receiver".into(), left_ty);
            let receiver = Expr::Ident("$pipe_receiver".into());
            match right.unspanned() {
                Expr::Call { callee, args } => {
                    let mut all_args = vec![receiver];
                    all_args.extend(args.iter().cloned());
                    self.check_call(callee, &all_args)
                }
                Expr::Ident(_) => self.check_call(right, &[receiver]),
                _ => Err(RillError::new("right side of |> must be callable")),
            }
        };
        self.env.pop();
        result
    }

    fn check_collection_callback(
        &self,
        name: &str,
        collection: &Type,
        callback: &Type,
    ) -> Result<(), RillError> {
        let arity = match collection {
            Type::Map if matches!(name, "map" | "parallel_map") => 2,
            Type::List(_) => 1,
            Type::Unknown => {
                if matches!(callback, Type::Function { arity: Some(2), .. })
                    && matches!(name, "map" | "parallel_map")
                {
                    return Ok(());
                }
                if matches!(name, "flat_map" | "find") {
                    return Ok(());
                }
                1
            }
            _ => return Err(RillError::new(format!("{name} cannot iterate {collection}"))),
        };
        if let Type::Function { arity: Some(actual), .. } = callback {
            if *actual != arity {
                return Err(RillError::new(format!(
                    "{name} callback needs {arity} arg, got {actual}"
                )));
            }
            return Ok(());
        }
        self.check_callback(name, callback.clone())
    }

    fn check_callback(&self, name: &str, callback_ty: Type) -> Result<(), RillError> {
        match callback_ty {
            Type::Function { arity: Some(1), .. } | Type::Unknown => Ok(()),
            Type::Function { arity: Some(arity), .. } => {
                Err(RillError::new(format!("{name} callback needs 1 arg, got {arity}")))
            }
            Type::Function { arity: None, .. } => Ok(()),
            other => Err(RillError::new(format!("{name} callback must be callable, got {other}"))),
        }
    }

    fn check_callback_expr(&mut self, expr: &Expr) -> Result<Type, RillError> {
        let result_type = self.check_expr(expr)?;
        if contains_implicit(expr) {
            Ok(Type::Function { arity: Some(1), params: None, return_type: Box::new(result_type) })
        } else {
            Ok(result_type)
        }
    }

    fn check_command(&mut self, command: &CommandExpr) -> Result<(), RillError> {
        command.validate_config()?;
        if command.argv.is_empty() {
            return Err(RillError::spanned("empty command", command.span));
        }
        for arg in &command.argv {
            self.check_command_arg(arg, command.span)?;
        }
        for redirect in &command.redirects {
            self.check_command_arg(&redirect.target, command.span)?;
        }
        for (name, expr) in &command.config {
            match name.as_str() {
                "cwd" => {
                    let ty = self.check_config_path(expr)?;
                    if !matches!(ty, Type::Str | Type::Path | Type::Unknown) {
                        return Err(RillError::spanned(
                            format!("command cwd config needs Str or Path, got {ty}"),
                            command.span,
                        ));
                    }
                }
                "env" => {
                    let ty = self.check_expr(expr)?;
                    if !matches!(ty, Type::Map | Type::Unknown) {
                        return Err(RillError::spanned(
                            format!("command env config needs Map, got {ty}"),
                            command.span,
                        ));
                    }
                }
                "stdin" | "stdout" | "stderr" => self.check_config_stream(expr, name)?,
                _ => {
                    return Err(RillError::spanned(
                        format!("unknown command config key '{name}'"),
                        command.span,
                    ));
                }
            }
        }
        Ok(())
    }

    fn check_config_path(&mut self, expr: &Expr) -> Result<Type, RillError> {
        if let Expr::Call { callee, args } = expr.unspanned() {
            if matches!(callee.unspanned(), Expr::Ident(name) if name == "file") {
                ensure_arity("file", 1, args.len())?;
                self.check_expr(&args[0])?;
                return Ok(Type::Path);
            }
        }
        self.check_expr(expr)
    }

    fn check_config_stream(&mut self, expr: &Expr, name: &str) -> Result<(), RillError> {
        if matches!(expr.unspanned(), Expr::Literal(Literal::Null)) {
            return Ok(());
        }
        if let Expr::Ident(value) = expr.unspanned() {
            if name == "stdin" && value == "capture" {
                return Err(RillError::new("stdin cannot be captured"));
            }
            if matches!(value.as_str(), "capture" | "inherit" | "null") {
                return Ok(());
            }
        }
        let ty = self.check_config_path(expr)?;
        if !matches!(ty, Type::Str | Type::Path | Type::Unknown) {
            return Err(RillError::new(format!(
                "command {name} config needs a stream mode or file path"
            )));
        }
        Ok(())
    }

    fn check_command_arg(
        &mut self,
        arg: &CommandArg,
        span: crate::diagnostic::Span,
    ) -> Result<(), RillError> {
        match arg {
            CommandArg::Var(name) => {
                let ty = self.env.get(name).ok_or_else(|| {
                    RillError::spanned(format!("unknown command variable '${name}'"), span)
                })?;
                if matches!(ty, Type::Function { .. }) {
                    return Err(RillError::spanned(
                        format!("cannot pass function '${name}' as argv"),
                        span,
                    ));
                }
            }
            CommandArg::Parts(parts) => {
                for part in parts {
                    if let CommandPart::Var(name) = part {
                        let ty = self.env.get(name).ok_or_else(|| {
                            RillError::spanned(format!("unknown command variable '${name}'"), span)
                        })?;
                        if matches!(ty, Type::Function { .. }) {
                            return Err(RillError::spanned(
                                format!("cannot pass function '${name}' as argv"),
                                span,
                            ));
                        }
                    }
                }
            }
            CommandArg::Expr(expr) => {
                self.check_expr(expr)?;
            }
            CommandArg::Splat(name) => {
                let ty = self.env.get(name).ok_or_else(|| {
                    RillError::spanned(format!("unknown command variable '${name}'"), span)
                })?;
                if matches!(ty, Type::Function { .. }) {
                    return Err(RillError::spanned(
                        format!("cannot pass function '${name}' as argv"),
                        span,
                    ));
                }
            }
            CommandArg::Word(_) | CommandArg::String(_) => {}
        }
        Ok(())
    }
}

fn check_typed_call(ty: &Type, args: &[Type]) -> Result<Type, RillError> {
    if let Type::Function { arity, params, return_type } = ty {
        if let Some(arity) = arity {
            ensure_arity("function", *arity, args.len())?;
        }
        if let Some(params) = params {
            for (expected, actual) in params.iter().zip(args) {
                ensure_assignable(expected, actual)?;
            }
        }
        Ok(*return_type.clone())
    } else {
        Ok(Type::Unknown)
    }
}

pub(crate) fn check_value_annotation(
    value: &Value,
    annotation: &TypeName,
) -> Result<(), RillError> {
    ensure_assignable(&type_from_name(annotation), &type_from_value(value))
}

fn top_level_names(stmts: &[Stmt]) -> Vec<String> {
    fn pattern_names(pattern: &LetPattern, names: &mut Vec<String>) {
        match pattern {
            LetPattern::Ident(name) if name != "_" => names.push(name.clone()),
            LetPattern::List(items) | LetPattern::Tuple(items) => {
                names.extend(items.iter().filter(|name| name.as_str() != "_").cloned());
            }
            LetPattern::Map(items) => names.extend(
                items.iter().map(|(key, alias)| alias.clone().unwrap_or_else(|| key.clone())),
            ),
            _ => {}
        }
    }

    let mut names = Vec::new();
    for stmt in stmts {
        match stmt {
            Stmt::Let { pattern, .. } => pattern_names(pattern, &mut names),
            Stmt::Fn { name, .. } => names.push(name.clone()),
            _ => {}
        }
    }
    names
}

fn type_from_literal(value: &Literal) -> Type {
    match value {
        Literal::Null => Type::Null,
        Literal::Bool(_) => Type::Bool,
        Literal::Int(_) => Type::Int,
        Literal::Float(_) => Type::Float,
        Literal::Duration(_) => Type::Duration,
        Literal::Str(_) => Type::Str,
        Literal::Regex(_) => Type::Regex,
    }
}

fn type_from_value(value: &Value) -> Type {
    match value {
        Value::Null => Type::Null,
        Value::Bool(_) => Type::Bool,
        Value::Int(_) => Type::Int,
        Value::Float(_) => Type::Float,
        Value::Str(_) => Type::Str,
        Value::Bytes(_) => Type::Bytes,
        Value::List(items) => Type::List(Box::new(
            items.iter().fold(Type::Unknown, |acc, item| common_type(acc, type_from_value(item))),
        )),
        Value::Tuple(items) => Type::Tuple(items.iter().map(type_from_value).collect()),
        Value::Map(_) => Type::Map,
        Value::Duration(_) => Type::Duration,
        Value::Path(_) => Type::Path,
        Value::Regex(_) => Type::Regex,
        Value::Error(_) => Type::Error,
        Value::Option(Some(value)) => Type::Option(Box::new(type_from_value(value))),
        Value::Option(None) => Type::Option(Box::new(Type::Unknown)),
        Value::Result(Ok(value)) => {
            Type::Result(Box::new(type_from_value(value)), Box::new(Type::Unknown))
        }
        Value::Result(Err(error)) => {
            Type::Result(Box::new(Type::Unknown), Box::new(type_from_value(error)))
        }
        Value::ProcessResult(_) => Type::ProcessResult,
        Value::ProcessHandle(_) => Type::ProcessHandle,
        Value::Stream(_) => Type::Stream,
        Value::Function(function) => Type::Function {
            arity: Some(function.params.len()),
            params: None,
            return_type: Box::new(Type::Unknown),
        },
        Value::Closure(closure) => Type::Function {
            arity: Some(closure.function.params.len()),
            params: None,
            return_type: Box::new(Type::Unknown),
        },
    }
}

fn method_kind(ty: &Type) -> crate::methods::Kind {
    use crate::methods::Kind;
    match ty {
        Type::Unknown => Kind::Unknown,
        Type::Null => Kind::Null,
        Type::Str => Kind::Str,
        Type::Float => Kind::Float,
        Type::List(_) => Kind::List,
        Type::Tuple(_) => Kind::Tuple,
        Type::Map => Kind::Map,
        Type::Path => Kind::Path,
        Type::Regex => Kind::Regex,
        Type::Duration => Kind::Duration,
        Type::ProcessHandle => Kind::ProcessHandle,
        Type::Int => Kind::Int,
        Type::Bool => Kind::Bool,
        Type::Function { .. } => Kind::Function,
        Type::Option(_) => Kind::Option,
        Type::Result(_, _) => Kind::Result,
        _ => Kind::Other,
    }
}

fn resolve_method_return(resolver: crate::methods::Return, receiver: &Type, args: &[Type]) -> Type {
    use crate::methods::Return;
    match resolver {
        Return::Unknown => Type::Unknown,
        Return::Null => Type::Null,
        Return::Bool => Type::Bool,
        Return::Int => Type::Int,
        Return::Float => Type::Float,
        Return::Str => Type::Str,
        Return::Map => Type::Map,
        Return::Path => Type::Path,
        Return::ProcessResult => Type::ProcessResult,
        Return::ProcessHandle => Type::ProcessHandle,
        Return::Receiver => receiver.clone(),
        Return::ReceiverItem => receiver.list_item(),
        Return::OptionalReceiverItem => Type::Option(Box::new(receiver.list_item())),
        Return::Join => match receiver {
            Type::List(_) | Type::Unknown => Type::Str,
            _ => Type::Path,
        },
        Return::CallbackList => {
            Type::List(Box::new(args.first().map(function_return).unwrap_or(Type::Unknown)))
        }
        Return::ListUnknown => Type::List(Box::new(Type::Unknown)),
        Return::ListTupleUnknown => {
            Type::List(Box::new(Type::Tuple(vec![Type::Int, Type::Unknown])))
        }
        Return::ListStr => Type::List(Box::new(Type::Str)),
        Return::ListPath => Type::List(Box::new(Type::Path)),
        Return::ListProcessResult => Type::List(Box::new(Type::ProcessResult)),
        Return::ListListStr => Type::List(Box::new(Type::List(Box::new(Type::Str)))),
        Return::ReadText => Type::Result(Box::new(Type::Str), Box::new(Type::Error)),
        Return::ReadYaml => Type::Result(Box::new(Type::Unknown), Box::new(Type::Error)),
        Return::ResultOkOption => match receiver {
            Type::Result(value, _) => Type::Option(value.clone()),
            _ => Type::Option(Box::new(Type::Unknown)),
        },
        Return::ResultErrOption => match receiver {
            Type::Result(_, error) => Type::Option(error.clone()),
            _ => Type::Option(Box::new(Type::Unknown)),
        },
        Return::Unwrapped => match receiver {
            Type::Result(value, _) | Type::Option(value) => *value.clone(),
            _ => Type::Unknown,
        },
        Return::OptionArgument => {
            Type::Option(Box::new(args.first().cloned().unwrap_or(Type::Unknown)))
        }
        Return::OkResultArgument => Type::Result(
            Box::new(args.first().cloned().unwrap_or(Type::Unknown)),
            Box::new(Type::Unknown),
        ),
        Return::ErrResultArgument => Type::Result(
            Box::new(Type::Unknown),
            Box::new(args.first().cloned().unwrap_or(Type::Unknown)),
        ),
    }
}

fn type_from_name(name: &TypeName) -> Type {
    match name {
        TypeName::Null => Type::Null,
        TypeName::Bool => Type::Bool,
        TypeName::Int => Type::Int,
        TypeName::Float => Type::Float,
        TypeName::Str => Type::Str,
        TypeName::Bytes => Type::Bytes,
        TypeName::ListAny => Type::List(Box::new(Type::Unknown)),
        TypeName::List(inner) => Type::List(Box::new(type_from_name(inner))),
        TypeName::Tuple(elements) => Type::Tuple(elements.iter().map(type_from_name).collect()),
        TypeName::Map => Type::Map,
        TypeName::Duration => Type::Duration,
        TypeName::Path => Type::Path,
        TypeName::Regex => Type::Regex,
        TypeName::Error => Type::Error,
        TypeName::Option(inner) => Type::Option(Box::new(type_from_name(inner))),
        TypeName::Result(inner, error) => Type::Result(
            Box::new(type_from_name(inner)),
            Box::new(error.as_ref().map(|ty| type_from_name(ty)).unwrap_or(Type::Unknown)),
        ),
        TypeName::ProcessResult => Type::ProcessResult,
        TypeName::ProcessHandle => Type::ProcessHandle,
        TypeName::Function { params, return_type } => Type::Function {
            arity: params.as_ref().map(|p| p.len()),
            params: params.as_ref().map(|p| p.iter().map(type_from_name).collect()),
            return_type: Box::new(
                return_type.as_ref().map(|rt| type_from_name(rt)).unwrap_or(Type::Unknown),
            ),
        },
    }
}

fn ensure_assignable(expected: &Type, actual: &Type) -> Result<(), RillError> {
    if expected.is_unknown() || actual.is_unknown() || expected == actual {
        return Ok(());
    }
    match (expected, actual) {
        (Type::Float, Type::Int) => Ok(()),
        (Type::Error, Type::Str) => Ok(()),
        (Type::List(expected), Type::List(actual)) => ensure_assignable(expected, actual),
        (Type::Tuple(expected), Type::Tuple(actual)) if expected.len() == actual.len() => {
            for (expected, actual) in expected.iter().zip(actual) {
                ensure_assignable(expected, actual)?;
            }
            Ok(())
        }
        (Type::Option(expected), Type::Option(actual)) => ensure_assignable(expected, actual),
        (Type::Result(expected, expected_error), Type::Result(actual, actual_error)) => {
            ensure_assignable(expected, actual)?;
            ensure_assignable(expected_error, actual_error)
        }
        (
            Type::Function {
                arity: expected_arity,
                params: expected_params,
                return_type: expected_rt,
            },
            Type::Function { arity: actual_arity, params: actual_params, return_type: actual_rt },
        ) => {
            if let (Some(ea), Some(aa)) = (expected_arity, actual_arity) {
                if ea != aa {
                    return Err(RillError::new(format!(
                        "function arity mismatch: expected {ea} params, got {aa}"
                    )));
                }
            }
            if let (Some(eps), Some(aps)) = (expected_params, actual_params) {
                for (ep, ap) in eps.iter().zip(aps.iter()) {
                    ensure_assignable(ep, ap)?;
                }
            }
            ensure_assignable(expected_rt, actual_rt)
        }
        _ => Err(RillError::new(format!("type mismatch: expected {expected}, got {actual}"))),
    }
}

fn ensure_arity(name: &str, expected: usize, actual: usize) -> Result<(), RillError> {
    if expected == actual {
        Ok(())
    } else {
        Err(RillError::new(format!("{name} needs {expected} args, got {actual}")))
    }
}

fn function_return(ty: &Type) -> Type {
    match ty {
        Type::Function { return_type, .. } => *return_type.clone(),
        _ => Type::Unknown,
    }
}

fn ensure_function_return(expected: &Type, actual: &Type) -> Result<(), RillError> {
    match (expected, actual) {
        (Type::Option(expected), Type::Option(actual)) => ensure_assignable(expected, actual),
        (Type::Option(expected), actual) => ensure_assignable(expected, actual),
        (Type::Result(expected, expected_error), Type::Result(actual, actual_error)) => {
            ensure_assignable(expected, actual)?;
            ensure_assignable(expected_error, actual_error)
        }
        (Type::Result(expected, _), actual) => ensure_assignable(expected, actual),
        _ => ensure_assignable(expected, actual),
    }
}

fn contains_implicit(expr: &Expr) -> bool {
    match expr.unspanned() {
        Expr::Implicit | Expr::ImplicitField(_) => true,
        Expr::List(items) => items.iter().any(contains_implicit),
        Expr::Tuple(items) => items.iter().any(contains_implicit),
        Expr::Map(pairs) => pairs.iter().any(|(_, value)| contains_implicit(value)),
        Expr::Call { callee, args } => {
            contains_implicit(callee) || args.iter().any(contains_implicit)
        }
        Expr::Field { base, .. } => contains_implicit(base),
        Expr::Index { base, index } => contains_implicit(base) || contains_implicit(index),
        Expr::Binary { left, right, .. } => contains_implicit(left) || contains_implicit(right),
        Expr::Pipe { left, right, .. } => contains_implicit(left) || contains_implicit(right),
        Expr::RegexMatch { value, .. }
        | Expr::RegexSubstitution { value, .. }
        | Expr::Match { value, .. } => contains_implicit(value),
        Expr::NamedArg { value, .. } => contains_implicit(value),
        Expr::Try(value) => contains_implicit(value),
        Expr::For { iter, .. } => contains_implicit(iter),
        Expr::Block(stmts) => stmts.iter().any(|stmt| match stmt {
            Stmt::Let { expr, .. } | Stmt::Expr(expr) | Stmt::Emit(expr) => contains_implicit(expr),
            Stmt::Assign { expr, .. } => contains_implicit(expr),
            Stmt::IndexAssign { index, expr, .. } => {
                contains_implicit(index) || contains_implicit(expr)
            }
            Stmt::While { cond, .. } => contains_implicit(cond),
            Stmt::If { cond, .. } => contains_implicit(cond),
            Stmt::On { host, .. } => contains_implicit(host),
            Stmt::Return(Some(expr)) => contains_implicit(expr),
            _ => false,
        }),
        Expr::Unary { operand, .. } => contains_implicit(operand),
        Expr::Closure { .. } | Expr::Literal(_) | Expr::Ident(_) | Expr::Command(_) => false,
        Expr::Located { expr, .. } => contains_implicit(expr),
    }
}

fn common_type(left: Type, right: Type) -> Type {
    if left == right {
        return left;
    }
    if left.is_unknown() {
        return right;
    }
    if right.is_unknown() {
        return left;
    }
    match (left, right) {
        (Type::Int, Type::Float) | (Type::Float, Type::Int) => Type::Float,
        (Type::List(left), Type::List(right)) => Type::List(Box::new(common_type(*left, *right))),
        (Type::Tuple(left), Type::Tuple(right)) if left.len() == right.len() => Type::Tuple(
            left.into_iter().zip(right).map(|(left, right)| common_type(left, right)).collect(),
        ),
        _ => Type::Unknown,
    }
}

fn constant_int(expr: &Expr) -> Option<i64> {
    match expr.unspanned() {
        Expr::Literal(Literal::Int(value)) => Some(*value),
        Expr::Binary { op: BinaryOp::Sub, left, right }
            if matches!(left.unspanned(), Expr::Literal(Literal::Int(0))) =>
        {
            match right.unspanned() {
                Expr::Literal(Literal::Int(value)) => value.checked_neg(),
                _ => None,
            }
        }
        _ => None,
    }
}

fn normalize_tuple_index(index: i64, len: usize) -> Result<usize, RillError> {
    let len_i64 = len as i64;
    let normalized = if index < 0 { len_i64 + index } else { index };
    if normalized < 0 || normalized >= len_i64 {
        Err(RillError::new(format!("tuple index {index} out of bounds (len {len})")))
    } else {
        Ok(normalized as usize)
    }
}

fn compound_assignment_type(op: AssignOp, current: &Type, value: &Type) -> Result<Type, RillError> {
    if op == AssignOp::Assign {
        return Ok(value.clone());
    }
    if current.is_unknown() || value.is_unknown() {
        return Ok(Type::Unknown);
    }
    match (op, current, value) {
        (AssignOp::Add, Type::Str, Type::Str) => Ok(Type::Str),
        (
            AssignOp::Add | AssignOp::Sub | AssignOp::Mul | AssignOp::Div,
            Type::Int | Type::Float,
            Type::Int | Type::Float,
        ) => Ok(common_type(current.clone(), value.clone())),
        _ => Err(RillError::new(format!(
            "compound assignment is not supported for {current} and {value}"
        ))),
    }
}

fn compile_regex(pattern: &str) -> Result<regex::Regex, RillError> {
    regex::Regex::new(pattern)
        .map_err(|err| RillError::new(format!("invalid regex /{pattern}/: {err}")))
}

fn regex_capture_names(pattern: &str) -> Result<Vec<String>, RillError> {
    let regex = compile_regex(pattern)?;
    Ok(regex.capture_names().flatten().map(ToString::to_string).collect())
}
