use crate::ast::*;
use crate::diagnostic::{RillError, Span};
use crate::types::Value;
use std::cell::Cell;
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
    /// A string-keyed map and the type of its values.
    Map(Box<Type>),
    Duration,
    Path,
    Regex,
    Error,
    Option(Box<Type>),
    Result(Box<Type>, Box<Type>),
    ProcessResult,
    ProcessHandle,
    Stream,
    Function {
        /// Maximum number of positional arguments.
        arity: Option<usize>,
        /// Minimum number of arguments (parameters without defaults).
        required: Option<usize>,
        params: Option<Vec<Type>>,
        names: Option<Vec<String>>,
        return_type: Box<Type>,
    },
}

impl Type {
    fn is_unknown(&self) -> bool {
        matches!(self, Type::Unknown)
    }

    fn is_numeric(&self) -> bool {
        matches!(self, Type::Int | Type::Float | Type::Unknown)
    }

    fn map_any() -> Type {
        Type::Map(Box::new(Type::Unknown))
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
            Type::Map(value) if value.is_unknown() => write!(f, "Map"),
            Type::Map(value) => write!(f, "Map[Str, {value}]"),
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
    /// Set once the binding is read; drives the unused-variable warning.
    used: Cell<bool>,
    /// Where a `let`/`var` declared it, when an unused binding should warn.
    declared: Option<Span>,
}

impl TypeBinding {
    fn new(ty: Type, mutable: bool) -> Self {
        Self { ty, mutable, used: Cell::new(false), declared: None }
    }
}

#[derive(Debug, Clone)]
struct Env {
    scopes: Vec<BTreeMap<String, TypeBinding>>,
}

impl Env {
    fn new() -> Self {
        let mut root = BTreeMap::new();
        let insert = |root: &mut BTreeMap<String, TypeBinding>, name: &str, ty: Type| {
            root.insert(name.to_string(), TypeBinding::new(ty, true));
        };
        insert(&mut root, "args", Type::List(Box::new(Type::Str)));
        insert(&mut root, "env", Type::Map(Box::new(Type::Str)));
        insert(&mut root, "input", Type::map_any());
        for builtin in crate::methods::BUILTINS {
            insert(
                &mut root,
                builtin.name,
                Type::Function {
                    required: None,
                    names: None,
                    arity: None,
                    params: None,
                    return_type: Box::new(Type::Unknown),
                },
            );
        }
        Self { scopes: vec![root] }
    }

    fn push(&mut self) {
        self.scopes.push(BTreeMap::new());
    }

    fn pop(&mut self) -> Option<BTreeMap<String, TypeBinding>> {
        self.scopes.pop()
    }

    fn set(&mut self, name: String, ty: Type) {
        self.scopes
            .last_mut()
            .expect("type env always has root scope")
            .insert(name, TypeBinding::new(ty, true));
    }

    /// Bind a name declared by a `let`/`var` statement; warns when never read.
    fn bind_declared(&mut self, name: String, ty: Type, mutable: bool, declared: Option<Span>) {
        let mut binding = TypeBinding::new(ty, mutable);
        binding.declared = declared;
        self.scopes.last_mut().expect("type env always has root scope").insert(name, binding);
    }

    /// Look a name up and mark it as read.
    fn get(&self, name: &str) -> Option<Type> {
        self.scopes.iter().rev().find_map(|scope| {
            scope.get(name).map(|binding| {
                binding.used.set(true);
                binding.ty.clone()
            })
        })
    }

    /// Whether a name is bound, without marking it as read.
    fn contains(&self, name: &str) -> bool {
        self.scopes.iter().any(|scope| scope.contains_key(name))
    }

    /// Mark a name as read without needing its type.
    fn touch(&self, name: &str) {
        if let Some(binding) = self.scopes.iter().rev().find_map(|scope| scope.get(name)) {
            binding.used.set(true);
        }
    }

    fn is_mutable(&self, name: &str) -> Option<bool> {
        self.scopes.iter().rev().find_map(|scope| scope.get(name).map(|b| b.mutable))
    }

    /// Index of the scope that declares `name`, counted from the root.
    fn scope_index(&self, name: &str) -> Option<usize> {
        self.scopes.iter().rposition(|scope| scope.contains_key(name))
    }

    /// Change the type of an existing binding in the scope that declares it.
    fn retype(&mut self, name: &str, ty: Type) {
        if let Some(binding) = self.scopes.iter_mut().rev().find_map(|scope| scope.get_mut(name)) {
            binding.ty = ty;
        }
    }

    /// Every name currently in scope, for suggestions.
    fn names(&self) -> Vec<&str> {
        let mut names: Vec<&str> =
            self.scopes.iter().flat_map(|scope| scope.keys().map(String::as_str)).collect();
        names.sort_unstable();
        names.dedup();
        names
    }
}

pub fn check(program: &Program) -> Result<(), RillError> {
    check_module(program, &[], false).map(|_| ())
}

/// Collect every error and warning in a program; errors are reported through
/// `check`, warnings are returned for `rill check` to print.
pub fn check_with_warnings(program: &Program) -> Result<Vec<RillError>, RillError> {
    check_module(program, &[], false).map(|checked| checked.warnings)
}

/// Upper bound on the errors reported for one module, to keep cascades short.
const MAX_ERRORS: usize = 20;

#[derive(Debug, Clone)]
pub(crate) struct CheckedModule {
    pub(crate) exports: ModuleExports,
    pub(crate) warnings: Vec<RillError>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ModuleExports {
    symbols: BTreeMap<String, Type>,
}

pub(crate) fn check_module(
    program: &Program,
    imports: &[&ModuleExports],
    is_module: bool,
) -> Result<CheckedModule, RillError> {
    let mut env = Env::new();
    for import in imports {
        for (name, ty) in &import.symbols {
            env.set(name.clone(), ty.clone());
        }
    }
    let mut checker = Checker {
        env,
        return_expected: None,
        try_kinds: Vec::new(),
        loop_depth: 0,
        loop_stack: Vec::new(),
        closure_param_hints: None,
        for_body_depth: 0,
        function_depth: 0,
        parallel_floor: None,
        errors: Vec::new(),
        warnings: Vec::new(),
        current_span: None,
        warn_unused_top_level: !is_module,
    };
    checker.check_program(program)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TryKind {
    Option,
    Result,
    Unknown,
}

struct Checker {
    env: Env,
    return_expected: Option<Type>,
    /// The kinds of `?` and `run!` seen in the function body being checked.
    try_kinds: Vec<TryKind>,
    loop_depth: usize,
    /// Enclosing loops, innermost last; `Some` collects `break` value types
    /// for a `loop {}` and `None` marks `while`/`for`.
    loop_stack: Vec<Option<Type>>,
    /// Parameter types for the next closure literal checked, inferred from the
    /// method or function it is passed to.
    closure_param_hints: Option<Vec<Type>>,
    /// While checking a parallel body: scopes below this index belong to the
    /// enclosing sequential code and may not be assigned to.
    parallel_floor: Option<usize>,
    for_body_depth: usize,
    function_depth: usize,
    /// Errors collected so far; checking continues past a failed statement.
    errors: Vec<RillError>,
    warnings: Vec<RillError>,
    /// Span of the statement being checked, for warnings without a better anchor.
    current_span: Option<Span>,
    /// Unused top-level `let`s warn in scripts but not in modules, whose
    /// top-level bindings are exports.
    warn_unused_top_level: bool,
}

/// Checker state restored after a statement fails, so later statements are
/// checked from a consistent position.
struct Snapshot {
    scopes: usize,
    loop_stack: usize,
    loop_depth: usize,
    for_body_depth: usize,
    function_depth: usize,
    parallel_floor: Option<usize>,
    return_expected: Option<Type>,
    try_kinds: usize,
}

impl Checker {
    fn snapshot(&self) -> Snapshot {
        Snapshot {
            scopes: self.env.scopes.len(),
            loop_stack: self.loop_stack.len(),
            loop_depth: self.loop_depth,
            for_body_depth: self.for_body_depth,
            function_depth: self.function_depth,
            parallel_floor: self.parallel_floor,
            return_expected: self.return_expected.clone(),
            try_kinds: self.try_kinds.len(),
        }
    }

    fn restore(&mut self, snapshot: Snapshot) {
        self.env.scopes.truncate(snapshot.scopes);
        self.loop_stack.truncate(snapshot.loop_stack);
        self.loop_depth = snapshot.loop_depth;
        self.for_body_depth = snapshot.for_body_depth;
        self.function_depth = snapshot.function_depth;
        self.parallel_floor = snapshot.parallel_floor;
        self.return_expected = snapshot.return_expected;
        self.try_kinds.truncate(snapshot.try_kinds);
        self.closure_param_hints = None;
    }

    fn record_error(&mut self, error: RillError) {
        let duplicate = self
            .errors
            .iter()
            .any(|known| known.message == error.message && known.span == error.span);
        if !duplicate && self.errors.len() < MAX_ERRORS {
            self.errors.push(error);
        }
    }

    fn warn(&mut self, span: Option<Span>, message: String, code: &'static str, help: &str) {
        let mut warning = RillError::warning(message, code).with_help(help);
        if let Some(span) = span {
            warning = warning.with_span(span);
        }
        self.warnings.push(warning);
    }

    fn pop_scope(&mut self) {
        if let Some(scope) = self.env.pop() {
            self.report_unused(&scope);
        }
    }

    fn report_unused(&mut self, scope: &BTreeMap<String, TypeBinding>) {
        for (name, binding) in scope {
            if binding.used.get() || name.starts_with('_') {
                continue;
            }
            if let Some(span) = binding.declared {
                self.warn(
                    Some(span),
                    format!("unused variable '{name}'"),
                    "rill::warn::unused_let",
                    "remove it, or name it `_` (or `_name`) to keep it on purpose",
                );
            }
        }
    }

    fn bind_pattern(
        &mut self,
        pattern: &LetPattern,
        ty: Type,
        mutable: bool,
        declared: Option<Span>,
    ) -> Result<(), RillError> {
        match pattern {
            LetPattern::Ident(name) => self.env.bind_declared(name.clone(), ty, mutable, declared),
            LetPattern::List(names) => {
                for name in names {
                    if name != "_" {
                        self.env.bind_declared(name.clone(), Type::Unknown, mutable, declared);
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
                        self.env.bind_declared(name.clone(), element, mutable, declared);
                    }
                }
            }
            LetPattern::Map(pairs) => {
                for (key, alias) in pairs {
                    let binding = alias.as_deref().unwrap_or(key.as_str());
                    self.env.bind_declared(binding.to_string(), Type::Unknown, mutable, declared);
                }
            }
        }
        Ok(())
    }

    fn check_program(&mut self, program: &Program) -> Result<CheckedModule, RillError> {
        for stmt in &program.stmts {
            if let Stmt::Fn { name, function } = stmt.unspanned() {
                self.env.set(
                    name.clone(),
                    Type::Function {
                        required: Some(
                            function.params.iter().filter(|p| p.default.is_none()).count(),
                        ),
                        names: Some(function.params.iter().map(|p| p.name.clone()).collect()),
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
        if self.warn_unused_top_level {
            let root = self.env.scopes.first().cloned().unwrap_or_default();
            self.report_unused(&root);
        }
        let mut symbols = BTreeMap::new();
        for name in top_level_names(&program.stmts) {
            if let Some(ty) = self.env.get(&name) {
                symbols.insert(name, ty);
            }
        }
        if !self.errors.is_empty() {
            let mut errors: Vec<RillError> = std::mem::take(&mut self.errors)
                .into_iter()
                .map(|error| error.with_kind(crate::types::ErrorKind::Type))
                .collect();
            let first = errors.remove(0);
            return Err(if errors.is_empty() { first } else { first.with_related(errors) });
        }
        let mut warnings = std::mem::take(&mut self.warnings);
        warnings.sort_by_key(|warning| warning.span.map(|span| span.start));
        Ok(CheckedModule { exports: ModuleExports { symbols }, warnings })
    }

    /// The type of a field/index chain used as an assignment target.
    fn check_place(&mut self, place: &Expr) -> Result<Type, RillError> {
        match place.unspanned() {
            Expr::Ident(name) => self.env.get(name).ok_or_else(|| self.unknown_identifier(name)),
            Expr::Field { base, name } => match self.check_place(base)? {
                Type::Map(value) => Ok(*value),
                Type::Unknown => Ok(Type::Unknown),
                other => Err(RillError::new(format!("cannot assign to field '{name}' of {other}"))),
            },
            Expr::Index { base, index } => {
                let base_ty = self.check_place(base)?;
                let index_ty = self.check_expr(index)?;
                match base_ty {
                    Type::List(item) => {
                        ensure_assignable(&Type::Int, &index_ty)?;
                        Ok(*item)
                    }
                    Type::Map(value) => {
                        if !matches!(index_ty, Type::Str | Type::Int | Type::Unknown) {
                            return Err(RillError::new(format!(
                                "map key must be Str, got {index_ty}"
                            )));
                        }
                        Ok(*value)
                    }
                    Type::Tuple(elements) => {
                        ensure_assignable(&Type::Int, &index_ty)?;
                        let raw_index = constant_int(index).ok_or_else(|| {
                            RillError::new("tuple assignment index must be an integer literal")
                        })?;
                        let position = normalize_tuple_index(raw_index, elements.len())?;
                        Ok(elements[position].clone())
                    }
                    Type::Unknown => Ok(Type::Unknown),
                    other => Err(RillError::new(format!("cannot assign into {other}"))),
                }
            }
            _ => Err(RillError::new("invalid assignment target")),
        }
    }

    /// Type-check parameter defaults against their annotations, inside the
    /// function scope so defaults may refer to earlier parameters.
    fn check_param_defaults(&mut self, function: &Function) -> Result<(), RillError> {
        for param in &function.params {
            let Some(default) = &param.default else { continue };
            let default_ty = self.check_expr(default)?;
            if let Some(annotation) = &param.type_name {
                ensure_assignable(&type_from_name(annotation), &default_ty).map_err(|error| {
                    match default.span() {
                        Some(span) => error.with_span(span),
                        None => error,
                    }
                })?;
            }
        }
        Ok(())
    }

    fn unknown_identifier(&self, name: &str) -> RillError {
        RillError::new(format!("unknown identifier '{name}'"))
            .with_optional_help(crate::suggest::did_you_mean(name, self.env.names()))
    }

    fn unknown_function(&self, name: &str) -> RillError {
        let help = if crate::methods::lookup(name).is_some() {
            Some(format!("`{name}` is a method; call it on a value as `value.{name}(...)`"))
        } else {
            crate::suggest::did_you_mean(name, self.env.names())
        };
        RillError::new(format!("unknown function '{name}'")).with_optional_help(help)
    }

    fn ensure_assignable_in_parallel(&self, name: &str) -> Result<(), RillError> {
        if let (Some(floor), Some(index)) = (self.parallel_floor, self.env.scope_index(name)) {
            if index < floor {
                return Err(RillError::new(format!(
                    "cannot assign to outer variable '{name}' from a parallel body; collect results with emit or parallel_map"
                )));
            }
        }
        Ok(())
    }

    /// Run `check` with the parallel-assignment floor set to the current scope depth.
    fn in_parallel_body<T>(
        &mut self,
        check: impl FnOnce(&mut Self) -> Result<T, RillError>,
    ) -> Result<T, RillError> {
        let previous = self.parallel_floor.replace(self.env.scopes.len());
        let result = check(self);
        self.parallel_floor = previous;
        result
    }

    /// Check a block.  A failing statement is recorded and checking continues
    /// from a restored state, so one run reports every independent error.
    fn check_stmts(&mut self, stmts: &[Stmt]) -> Result<Type, RillError> {
        let mut last = Type::Null;
        let mut terminated_by: Option<&'static str> = None;
        // The type of the first statement that leaves the block; statements
        // after it never run and must not decide the block's type.
        let mut terminal_type: Option<Type> = None;
        for (index, stmt) in stmts.iter().enumerate() {
            if let Some(keyword) = terminated_by.take() {
                self.warn(
                    stmt.span(),
                    "unreachable statement".to_string(),
                    "rill::warn::unreachable",
                    &format!("the `{keyword}` above leaves this block before it runs"),
                );
            }
            if terminal_type.is_some() {
                // Still check unreachable code for errors, but ignore its type.
                let snapshot = self.snapshot();
                if let Err(error) = self.check_stmt(stmt) {
                    self.record_error(error);
                    self.restore(snapshot);
                }
                continue;
            }
            let snapshot = self.snapshot();
            match self.check_stmt(stmt) {
                Ok(ty) => {
                    let is_last = index + 1 == stmts.len();
                    if !is_last && matches!(stmt.unspanned(), Stmt::Expr(_)) {
                        if let Type::Result(..) = ty {
                            self.warn(
                                stmt.span(),
                                "unused Result".to_string(),
                                "rill::warn::unused_result",
                                "handle it with `?`, `match`, or `.unwrap_or(...)`; write `let _ = ...` to discard it on purpose",
                            );
                        }
                    }
                    last = ty;
                }
                Err(error) => {
                    self.record_error(error);
                    self.restore(snapshot);
                    if let Stmt::Let { pattern, mutable, .. } = stmt.unspanned() {
                        // Poison the names so later uses do not cascade.
                        let _ = self.bind_pattern(pattern, Type::Unknown, *mutable, None);
                    }
                    last = Type::Unknown;
                }
            }
            terminated_by = terminator(stmt);
            if terminated_by.is_some() {
                terminal_type = Some(last.clone());
            }
        }
        Ok(terminal_type.unwrap_or(last))
    }

    fn check_stmt(&mut self, stmt: &Stmt) -> Result<Type, RillError> {
        match stmt {
            Stmt::Located { stmt, span } => {
                let previous = self.current_span.replace(*span);
                let checked = self.check_stmt(stmt).map_err(|error| error.with_span(*span));
                self.current_span = previous;
                checked
            }
            Stmt::Use(module) => {
                if !crate::methods::NAMESPACES.contains(&module.as_str())
                    && !module.starts_with("./")
                    && !module.starts_with("../")
                {
                    return Err(RillError::new(format!("unknown module '{module}'"))
                        .with_optional_help(crate::suggest::did_you_mean(
                            module,
                            crate::methods::NAMESPACES.iter().copied(),
                        )));
                }
                Ok(Type::Null)
            }
            Stmt::Let { pattern, type_name, expr, mutable } => {
                let declared = self.current_span;
                if let Expr::If { else_body, .. } = expr.unspanned() {
                    if else_body.is_empty() {
                        return Err(RillError::new("an if used as a value needs an else branch")
                            .with_help("add `else { ... }`, or use a statement instead"));
                    }
                }
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
                self.bind_pattern(pattern, ty, *mutable, declared)?;
                Ok(Type::Null)
            }
            Stmt::AssignEnv { name: _, expr } => {
                let ty = self.check_expr(expr)?;
                ensure_assignable(&Type::Str, &ty)?;
                Ok(Type::Null)
            }
            Stmt::Assign { target, op, expr } => {
                let Some(root) = place_root(target) else {
                    return Err(RillError::new("invalid assignment target").with_help(
                        "assign to a variable, a field such as `m.key`, or an index such as `xs[0]`",
                    ));
                };
                let root = root.to_string();
                self.ensure_assignable_in_parallel(&root)?;
                if matches!(self.env.is_mutable(&root), Some(false)) {
                    return Err(RillError::new(format!(
                        "cannot assign to immutable variable '{root}'"
                    ))
                    .with_help(format!(
                        "declare it with `var {root} = ...` to allow reassignment"
                    )));
                }
                let Some(current) = self.env.get(&root) else {
                    return Err(RillError::new(format!(
                        "assignment to undeclared variable '{root}'"
                    ))
                    .with_help(
                        crate::suggest::did_you_mean(&root, self.env.names())
                            .unwrap_or_else(|| format!("declare it first with `var {root} = ...`")),
                    ));
                };
                if !matches!(target.unspanned(), Expr::Ident(_)) {
                    let target_ty = self.check_place(target)?;
                    let value_ty = self.check_expr(expr)?;
                    let assigned = compound_assignment_type(*op, &target_ty, &value_ty)?;
                    if !target_ty.is_unknown() {
                        ensure_assignable(&target_ty, &assigned)?;
                    }
                    return Ok(Type::Null);
                }
                let value_ty = self.check_expr(expr)?;
                let assigned = compound_assignment_type(*op, &current, &value_ty)?;
                if matches!(current, Type::Null | Type::Unknown)
                    || ensure_assignable(&current, &assigned).is_ok()
                {
                    if !assigned.is_unknown() && current != assigned {
                        self.env.retype(&root, common_type(current, assigned));
                    }
                } else if ensure_assignable(&assigned, &current).is_ok() {
                    // Widening (Int -> Float): the variable takes the wider type.
                    self.env.retype(&root, assigned);
                } else {
                    return Err(RillError::new(format!(
                        "cannot assign {assigned} to '{root}', which has type {current}"
                    ))
                    .with_help(format!(
                        "declare a new variable, or convert with `{current}(...)`"
                    )));
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
                self.env.set("args".to_string(), Type::map_any());
                Ok(Type::Null)
            }
            Stmt::InputLines => Ok(Type::Null),
            Stmt::Fn { name, function } => {
                self.env.set(
                    name.clone(),
                    Type::Function {
                        required: Some(
                            function.params.iter().filter(|p| p.default.is_none()).count(),
                        ),
                        names: Some(function.params.iter().map(|p| p.name.clone()).collect()),
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
                let previous_loop_stack = std::mem::take(&mut self.loop_stack);
                let previous_tries = std::mem::take(&mut self.try_kinds);
                self.function_depth += 1;
                self.return_expected = function.return_type.as_ref().map(type_from_name);
                let checked = self
                    .check_param_defaults(function)
                    .and_then(|()| self.check_stmts(&function.body))
                    .and_then(|body_ty| {
                    if let Some(return_type) = &function.return_type {
                        let declared = type_from_name(return_type);
                        let last_span = function.body.last().and_then(|s| match s.unspanned() {
                            Stmt::Return(Some(e)) | Stmt::Expr(e) | Stmt::Emit(e) => e.span(),
                            Stmt::Let { expr, .. } => expr.span(),
                            _ => None,
                        });
                        ensure_function_return(&declared, &body_ty).map_err(|e| match last_span {
                            Some(s) => e.with_span(s),
                            None => e,
                        })?;
                        if !matches!(declared, Type::Null | Type::Unknown)
                            && !stmts_always_yield(&function.body)
                        {
                            return Err(RillError::new(format!(
                                "function '{name}' is declared to return {declared} but can finish without a value"
                            ))
                            .with_help("add a `return` or a final expression on every path"));
                        }
                    }
                    Ok(body_ty)
                });
                let tries = std::mem::replace(&mut self.try_kinds, previous_tries);
                self.return_expected = previous_return;
                self.loop_depth = previous_loop;
                self.loop_stack = previous_loop_stack;
                self.function_depth -= 1;
                self.pop_scope();
                let body_ty = checked?;
                let body_ty = infer_propagation(
                    &function.propagation,
                    function.return_type.is_some(),
                    &tries,
                    body_ty,
                );
                self.env.set(
                    name.clone(),
                    Type::Function {
                        required: Some(
                            function.params.iter().filter(|p| p.default.is_none()).count(),
                        ),
                        names: Some(function.params.iter().map(|p| p.name.clone()).collect()),
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
            Stmt::WithEnv { bindings, body } => {
                for (_, expr) in bindings {
                    let ty = self.check_expr(expr)?;
                    ensure_assignable(&Type::Str, &ty)?;
                }
                self.env.push();
                self.check_stmts(body)?;
                self.pop_scope();
                Ok(Type::Null)
            }
            Stmt::On { host, body } => {
                let host_ty = self.check_expr(host)?;
                if !matches!(host_ty, Type::Str | Type::List(_) | Type::Unknown) {
                    return Err(RillError::new(format!("on needs Str or List, got {host_ty}")));
                }
                self.env.push();
                self.check_stmts(body)?;
                self.pop_scope();
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
            Stmt::Continue => {
                if self.loop_depth == 0 {
                    return Err(RillError::new("break/continue outside loop"));
                }
                Ok(Type::Null)
            }
            Stmt::Break(value) => {
                if self.loop_depth == 0 {
                    return Err(RillError::new("break/continue outside loop"));
                }
                if let Some(value) = value {
                    let ty = self.check_expr(value)?;
                    match self.loop_stack.last_mut() {
                        Some(Some(collected)) => {
                            *collected = common_type(collected.clone(), ty);
                        }
                        _ => {
                            return Err(RillError::new(
                                "break with a value is only valid inside loop { }",
                            )
                            .with_help("while and for loops do not produce a value from break"))
                        }
                    }
                }
                Ok(Type::Null)
            }
            Stmt::While { cond, body } => {
                let cond_ty = self.check_expr(cond)?;
                if !matches!(cond_ty, Type::Bool | Type::Unknown) {
                    return Err(RillError::new(format!(
                        "while condition must be Bool, got {cond_ty}"
                    )));
                }
                self.loop_depth += 1;
                self.loop_stack.push(None);
                let checked = self.check_stmts(body);
                self.loop_stack.pop();
                self.loop_depth -= 1;
                checked?;
                Ok(Type::Null)
            }
            Stmt::Expr(expr) => self.check_expr(expr),
        }
    }

    fn warn_shadowed_capture(&mut self, name: &str, span: Option<Span>) {
        if self.env.contains(name) {
            self.warn(
                span,
                format!("regex capture '{name}' shadows an existing binding"),
                "rill::warn::shadowed_capture",
                "rename the capture group, or the variable, so the two cannot be confused",
            );
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
                self.env.get(name).ok_or_else(|| self.unknown_identifier(name))
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
                let mut value_ty = Type::Unknown;
                for (_, expr) in pairs {
                    value_ty = common_type(value_ty, self.check_expr(expr)?);
                }
                Ok(Type::Map(Box::new(value_ty)))
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
            Expr::Slice { base, start, end, .. } => {
                let base_ty = self.check_expr(base)?;
                for bound in [start, end].into_iter().flatten() {
                    let bound_ty = self.check_expr(bound)?;
                    if !matches!(bound_ty, Type::Int | Type::Unknown) {
                        return Err(RillError::new(format!(
                            "slice bounds must be Int, got {bound_ty}"
                        )));
                    }
                }
                match base_ty {
                    Type::List(_) | Type::Str | Type::Unknown => Ok(base_ty),
                    Type::Tuple(_) => Err(RillError::new("cannot slice a Tuple")
                        .with_help("tuples have a fixed shape; index them instead")),
                    other => Err(RillError::new(format!("cannot slice {other}"))),
                }
            }
            Expr::Field { base, name } => self.check_field(base, name),
            Expr::Binary { op, left, right } => self.check_binary(*op, left, right),
            Expr::Pipe { kind, left, right } => self.check_pipe(*kind, left, right),
            Expr::Command(command) => {
                if command.check_exit {
                    self.try_kinds.push(TryKind::Result);
                }
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
                Type::Option(value) => {
                    self.try_kinds.push(TryKind::Option);
                    Ok(*value)
                }
                Type::Result(value, _) => {
                    self.try_kinds.push(TryKind::Result);
                    Ok(*value)
                }
                Type::Unknown => {
                    self.try_kinds.push(TryKind::Unknown);
                    Ok(Type::Unknown)
                }
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
                if !matches!(iter_ty, Type::List(_) | Type::Map(_) | Type::Stream | Type::Unknown) {
                    return Err(RillError::new(format!(
                        "for loop needs List, Map, or Stream, got {iter_ty}"
                    )));
                }
                let parallel = !matches!(mode, ForMode::Sequential);
                let previous_floor = if parallel {
                    self.parallel_floor.replace(self.env.scopes.len())
                } else {
                    self.parallel_floor
                };
                self.env.push();
                let item_ty = match iter_ty {
                    Type::Stream => Type::map_any(),
                    Type::Map(value) => Type::Tuple(vec![Type::Str, *value]),
                    other => other.list_item(),
                };
                self.bind_pattern(pattern, item_ty, true, None)?;
                self.loop_depth += 1;
                self.loop_stack.push(None);
                self.for_body_depth += 1;
                let checked = self.check_stmts(body);
                self.for_body_depth -= 1;
                self.loop_stack.pop();
                self.loop_depth -= 1;
                self.pop_scope();
                self.parallel_floor = previous_floor;
                checked?;
                Ok(match mode {
                    ForMode::Sequential | ForMode::Parallel => Type::List(Box::new(Type::Unknown)),
                    ForMode::Race => Type::Unknown,
                })
            }
            Expr::If { cond, pattern, then_body, else_body } => {
                let cond_ty = self.check_expr(cond)?;
                if pattern.is_none() && !matches!(cond_ty, Type::Bool | Type::Unknown) {
                    return Err(RillError::new(format!(
                        "if condition must be Bool, got {cond_ty}"
                    )));
                }
                self.env.push();
                if let Some(pattern) = pattern {
                    self.bind_pattern(pattern, cond_ty.clone(), true, None)?;
                }
                if let Expr::RegexMatch { pattern, .. } = cond.unspanned() {
                    for name in regex_capture_names(pattern)? {
                        self.warn_shadowed_capture(&name, cond.span());
                        self.env.set(name, Type::Str);
                    }
                }
                let then_ty = self.check_stmts(then_body)?;
                self.pop_scope();
                self.env.push();
                let else_ty = self.check_stmts(else_body)?;
                self.pop_scope();
                Ok(common_type(then_ty, else_ty))
            }
            Expr::Loop { body } => {
                self.loop_depth += 1;
                self.loop_stack.push(Some(Type::Unknown));
                self.env.push();
                let checked = self.check_stmts(body);
                self.pop_scope();
                let breaks = self.loop_stack.pop().flatten().unwrap_or(Type::Unknown);
                self.loop_depth -= 1;
                checked?;
                Ok(breaks)
            }
            Expr::Block(stmts) => {
                self.env.push();
                let ty = self.check_stmts(stmts)?;
                self.pop_scope();
                Ok(ty)
            }
            Expr::Closure { params, body, propagation } => {
                let previous_loop = std::mem::replace(&mut self.loop_depth, 0);
                let previous_loop_stack = std::mem::take(&mut self.loop_stack);
                let previous_return = self.return_expected.take();
                let previous_tries = std::mem::take(&mut self.try_kinds);
                self.return_expected = Some(Type::Unknown);
                self.function_depth += 1;
                self.env.push();
                let hints = self.closure_param_hints.take().unwrap_or_default();
                for (index, param) in params.iter().enumerate() {
                    self.env.set(param.clone(), hints.get(index).cloned().unwrap_or(Type::Unknown));
                }
                let return_type = self.check_stmts(body);
                let tries = std::mem::replace(&mut self.try_kinds, previous_tries);
                self.loop_depth = previous_loop;
                self.loop_stack = previous_loop_stack;
                self.return_expected = previous_return;
                self.function_depth -= 1;
                self.pop_scope();
                let return_type = infer_propagation(propagation, false, &tries, return_type?);
                Ok(Type::Function {
                    required: Some(params.len()),
                    names: Some(params.clone()),
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
            Expr::Unary { op: UnaryOp::Neg, operand } => {
                let ty = self.check_expr(operand)?;
                match ty {
                    Type::Int | Type::Float | Type::Unknown => Ok(ty),
                    other => {
                        Err(RillError::new(format!("unary minus needs a number, got {other}")))
                    }
                }
            }
            Expr::Implicit => Ok(self.env.get("it").unwrap_or(Type::Unknown)),
            Expr::ImplicitField(_) => Ok(Type::Unknown),
            Expr::Interpolated(parts) => {
                self.check_parts(parts)?;
                Ok(Type::Str)
            }
        }
    }

    fn check_parts(&mut self, parts: &[StrPart]) -> Result<(), RillError> {
        for part in parts {
            match part {
                StrPart::Text(_) => {}
                StrPart::Var(name) => self.env.touch(name),
                StrPart::VarDefault(name, default) => {
                    self.env.touch(name);
                    self.check_parts(default)?;
                }
                StrPart::Expr(expr) => {
                    let ty = self.check_expr(expr)?;
                    if matches!(ty, Type::Function { .. }) {
                        return Err(RillError::new("cannot interpolate a function into a string")
                            .with_span(expr.span().unwrap_or(Span::new(0, 0))));
                    }
                }
            }
        }
        Ok(())
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
                    return Err(unknown_namespace_function(module, name));
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
        self.pop_scope();
        result
    }

    fn check_method(&mut self, base: &Expr, name: &str, args: &[Expr]) -> Result<Type, RillError> {
        let receiver = self.check_expr(base)?;

        let kind = method_kind(&receiver);
        let method = crate::methods::lookup(name)
            .ok_or_else(|| RillError::new(format!("unknown method .{name}()")))?;
        method.validate(kind, args.len()).map_err(|message| {
            let error = RillError::new(message);
            match receiver {
                Type::Result(..) => error.with_help(
                    "this value is a Result; unwrap it first with `?`, `.unwrap()`, or a match",
                ),
                Type::Option(..) => error.with_help(
                    "this value is an Option; unwrap it first with `?`, `.unwrap_or(...)`, or a match",
                ),
                _ => error,
            }
        })?;
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
                let hints = callback_param_types(name, &receiver);
                self.env.push();
                self.env.set("it".into(), hints.first().cloned().unwrap_or(Type::Unknown));
                if matches!(arg.unspanned(), Expr::Closure { .. }) {
                    self.closure_param_hints = Some(hints);
                }
                let ty = if matches!(name, "parallel_map" | "parallel_filter") {
                    self.in_parallel_body(|checker| checker.check_callback_expr(arg))
                } else {
                    self.check_callback_expr(arg)
                };
                self.closure_param_hints = None;
                self.pop_scope();
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
        if !matches!(callee_ty, Type::Function { .. }) {
            return Err(RillError::new(format!("{callee_ty} is not callable")));
        }
        self.check_call_against("function", &callee_ty, args)
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
        let ty = self.env.get(name).ok_or_else(|| self.unknown_function(name))?;
        if !matches!(ty, Type::Function { .. }) {
            return Err(RillError::new(format!("{name} is not callable")));
        }
        self.check_call_against(name, &ty, args)
    }

    /// Check positional and named arguments against a function type.
    fn check_call_against(
        &mut self,
        name: &str,
        ty: &Type,
        args: &[Expr],
    ) -> Result<Type, RillError> {
        let Type::Function { arity, required, params, names, return_type } = ty else {
            return Ok(Type::Unknown);
        };
        let (positional, named) = split_call_args(args)?;
        if let Some(arity) = arity {
            let required = required.unwrap_or(*arity);
            if positional.len() > *arity || named.is_empty() {
                ensure_arity_range(name, required, *arity, positional.len())?;
            }
        }
        let mut provided = vec![false; arity.unwrap_or(positional.len())];
        for (index, arg) in positional.iter().enumerate() {
            if let (Expr::Closure { .. }, Some(Type::Function { params: Some(hints), .. })) =
                (arg.unspanned(), params.as_ref().and_then(|p| p.get(index)))
            {
                self.closure_param_hints = Some(hints.clone());
            }
            let actual = self.check_expr(arg);
            self.closure_param_hints = None;
            let actual = actual?;
            if let Some(expected) = params.as_ref().and_then(|p| p.get(index)) {
                ensure_assignable(expected, &actual)?;
            }
            if let Some(slot) = provided.get_mut(index) {
                *slot = true;
            }
        }
        for (label, value) in named {
            let actual = self.check_expr(value)?;
            let Some(names) = names else {
                continue;
            };
            let Some(index) = names.iter().position(|n| n == label) else {
                return Err(RillError::new(format!("{name} has no parameter '{label}'"))
                    .with_optional_help(crate::suggest::did_you_mean(
                        label,
                        names.iter().map(String::as_str),
                    )));
            };
            if provided[index] {
                return Err(RillError::new(format!("parameter '{label}' given twice")));
            }
            provided[index] = true;
            if let Some(expected) = params.as_ref().and_then(|p| p.get(index)) {
                ensure_assignable(expected, &actual)?;
            }
        }
        if let (Some(required), Some(names)) = (required, names) {
            if let Some(index) = provided.iter().take(*required).position(|given| !given) {
                return Err(RillError::new(format!(
                    "{name} is missing an argument for parameter '{}'",
                    names[index]
                )));
            }
        }
        Ok((**return_type).clone())
    }

    fn check_match(&mut self, value: &Expr, arms: &[MatchArm]) -> Result<Type, RillError> {
        let value_ty = self.check_expr(value)?;
        let mut result_ty = Type::Unknown;
        let mut coverage = Coverage::default();
        for arm in arms {
            self.env.push();
            let checked = self.check_pattern(&arm.pattern, &value_ty).and_then(|bindings| {
                for (name, ty) in bindings {
                    self.env.set(name, ty);
                }
                if let Some(guard) = &arm.guard {
                    let guard_ty = self.check_expr(guard)?;
                    if !matches!(guard_ty, Type::Bool | Type::Unknown) {
                        return Err(RillError::new(format!(
                            "match guard must be Bool, got {guard_ty}"
                        )));
                    }
                } else {
                    coverage.add(&arm.pattern);
                }
                self.check_expr(&arm.expr)
            });
            self.pop_scope();
            result_ty = common_type(result_ty, checked?);
        }
        if !coverage.complete(&value_ty) {
            let missing = match &value_ty {
                Type::Bool => "add arms for `true` and `false`, or `_`",
                Type::Option(_) => "add a `none` arm, or `_`",
                Type::Result(..) => "add `ok(_)` and `err(_)` arms, or `_`",
                Type::Null => "add a `null` arm, or `_`",
                _ => {
                    // Other types may fall through to null, as before.
                    return Ok(common_type(result_ty, Type::Null));
                }
            };
            return Err(
                RillError::new(format!("non-exhaustive match on {value_ty}")).with_help(missing)
            );
        }
        Ok(result_ty)
    }

    /// Check a pattern against the matched value's type and return the names
    /// it binds with their types.
    fn check_pattern(
        &mut self,
        pattern: &Pattern,
        value_ty: &Type,
    ) -> Result<Vec<(String, Type)>, RillError> {
        let unknown = value_ty.is_unknown();
        match pattern {
            Pattern::Wildcard => Ok(Vec::new()),
            Pattern::Bind(name) => Ok(vec![(name.clone(), value_ty.clone())]),
            Pattern::Literal(literal) => {
                let literal_ty = type_from_literal(literal);
                let compatible = unknown
                    || match (&literal_ty, value_ty) {
                        (Type::Int | Type::Float, Type::Int | Type::Float) => true,
                        (Type::Null, Type::Option(_)) => false,
                        (left, right) => left == right,
                    };
                if !compatible {
                    return Err(RillError::new(format!(
                        "pattern of type {literal_ty} cannot match a {value_ty}"
                    )));
                }
                Ok(Vec::new())
            }
            Pattern::Regex(regex) => {
                if !matches!(value_ty, Type::Str | Type::Unknown) {
                    return Err(RillError::new(format!(
                        "regex match arm requires Str value, got {value_ty}"
                    )));
                }
                let names = regex_capture_names(regex)?;
                for name in &names {
                    self.warn_shadowed_capture(name, self.current_span);
                }
                Ok(names.into_iter().map(|name| (name, Type::Str)).collect())
            }
            Pattern::Tuple(items) => {
                let element_types = match value_ty {
                    Type::Tuple(elements) if elements.len() == items.len() => elements.clone(),
                    Type::Tuple(elements) => {
                        return Err(RillError::new(format!(
                            "tuple pattern has {} elements but the value has {}",
                            items.len(),
                            elements.len()
                        )))
                    }
                    Type::Unknown => vec![Type::Unknown; items.len()],
                    other => {
                        return Err(RillError::new(format!("tuple pattern cannot match a {other}")))
                    }
                };
                let mut bindings = Vec::new();
                for (item, element_ty) in items.iter().zip(&element_types) {
                    bindings.extend(self.check_pattern(item, element_ty)?);
                }
                Ok(bindings)
            }
            Pattern::List { items, .. } => {
                let item_ty = match value_ty {
                    Type::List(item) => (**item).clone(),
                    Type::Unknown => Type::Unknown,
                    other => {
                        return Err(RillError::new(format!("list pattern cannot match a {other}")))
                    }
                };
                let mut bindings = Vec::new();
                for item in items {
                    bindings.extend(self.check_pattern(item, &item_ty)?);
                }
                Ok(bindings)
            }
            Pattern::Some(inner) => match value_ty {
                Type::Option(value) => self.check_pattern(inner, value),
                Type::Unknown => self.check_pattern(inner, &Type::Unknown),
                other => Err(RillError::new(format!("some pattern cannot match a {other}"))),
            },
            Pattern::None => match value_ty {
                Type::Option(_) | Type::Unknown => Ok(Vec::new()),
                other => Err(RillError::new(format!("none pattern cannot match a {other}"))),
            },
            Pattern::Ok(inner) => match value_ty {
                Type::Result(value, _) => self.check_pattern(inner, value),
                Type::Unknown => self.check_pattern(inner, &Type::Unknown),
                other => Err(RillError::new(format!("ok pattern cannot match a {other}"))),
            },
            Pattern::Err(inner) => match value_ty {
                Type::Result(_, error) => self.check_pattern(inner, error),
                Type::Unknown => self.check_pattern(inner, &Type::Unknown),
                other => Err(RillError::new(format!("err pattern cannot match a {other}"))),
            },
            Pattern::Or(alternatives) => {
                let mut merged: Option<Vec<(String, Type)>> = None;
                for alternative in alternatives {
                    let mut bindings = self.check_pattern(alternative, value_ty)?;
                    bindings.sort_by(|a, b| a.0.cmp(&b.0));
                    match &mut merged {
                        None => merged = Some(bindings),
                        Some(existing) => {
                            let same_names = existing.len() == bindings.len()
                                && existing.iter().zip(&bindings).all(|(a, b)| a.0 == b.0);
                            if !same_names {
                                return Err(RillError::new(
                                    "every alternative of an or-pattern must bind the same names",
                                ));
                            }
                            for (slot, (_, ty)) in existing.iter_mut().zip(bindings) {
                                slot.1 = common_type(slot.1.clone(), ty);
                            }
                        }
                    }
                }
                Ok(merged.unwrap_or_default())
            }
        }
    }

    fn check_field(&mut self, base: &Expr, name: &str) -> Result<Type, RillError> {
        if let Expr::Ident(module) = base.unspanned() {
            if crate::methods::is_namespace(module) && self.env.get(module).is_none() {
                if module == "Stream" && name == "tail" {
                    return Ok(Type::Function {
                        required: None,
                        names: None,
                        arity: None,
                        params: None,
                        return_type: Box::new(Type::Stream),
                    });
                }
                if crate::methods::namespace_method(module, name).is_some() {
                    return Ok(Type::Function {
                        required: None,
                        names: None,
                        arity: None,
                        params: None,
                        return_type: Box::new(Type::Unknown),
                    });
                }
                return Err(unknown_namespace_function(module, name));
            }
        }
        let base_ty = self.check_expr(base)?;
        match base_ty {
            Type::ProcessResult => match name {
                "status" => Ok(Type::Int),
                "signal" => Ok(Type::Option(Box::new(Type::Int))),
                "out" | "err" => Ok(Type::Str),
                "ok" => Ok(Type::Bool),
                _ => Err(RillError::new(format!("ProcessResult has no field '{name}'"))
                    .with_optional_help(crate::suggest::did_you_mean(
                        name,
                        ["status", "signal", "out", "err", "ok"],
                    ))),
            },
            Type::Map(value) => Ok(*value),
            Type::Unknown => Ok(Type::Unknown),
            ref ty
                if crate::methods::lookup(name)
                    .is_some_and(|method| method.accepts(method_kind(ty))) =>
            {
                Err(RillError::new(format!(".{name} is a method; call .{name}(...)")))
            }
            other => Err(unknown_member(&other, name)),
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
                let orderable = left_ty.is_unknown()
                    || right_ty.is_unknown()
                    || (left_ty.is_numeric() && right_ty.is_numeric())
                    || (left_ty == Type::Str && right_ty == Type::Str)
                    || (left_ty == Type::Duration && right_ty == Type::Duration);
                if !orderable {
                    return Err(RillError::new(format!(
                        "comparison needs two numbers, two strings, or two durations, got {left_ty} and {right_ty}"
                    )));
                }
                Ok(Type::Bool)
            }
            BinaryOp::In | BinaryOp::NotIn => {
                match &right_ty {
                    Type::List(_) | Type::Map(_) | Type::Str | Type::Unknown => {}
                    other => {
                        return Err(RillError::new(format!(
                            "`in` needs a List, Map, or Str on the right, got {other}"
                        )))
                    }
                }
                if matches!(right_ty, Type::Map(_) | Type::Str)
                    && !matches!(left_ty, Type::Str | Type::Unknown)
                {
                    return Err(RillError::new(format!(
                        "`in` on a {right_ty} needs a Str on the left, got {left_ty}"
                    )));
                }
                Ok(Type::Bool)
            }
            BinaryOp::Coalesce => Ok(match left_ty {
                Type::Option(inner) => common_type(*inner, right_ty),
                Type::Null => right_ty,
                other => common_type(other, right_ty),
            }),
            BinaryOp::Add if left_ty == Type::Str && right_ty == Type::Str => Ok(Type::Str),
            BinaryOp::Add if matches!((&left_ty, &right_ty), (Type::List(_), Type::List(_))) => {
                Ok(common_type(left_ty, right_ty))
            }
            BinaryOp::Add if left_ty == Type::Unknown || right_ty == Type::Unknown => {
                Ok(Type::Unknown)
            }
            BinaryOp::Add
            | BinaryOp::Sub
            | BinaryOp::Mul
            | BinaryOp::Div
            | BinaryOp::Rem
            | BinaryOp::Pow
                if left_ty == Type::Duration || right_ty == Type::Duration =>
            {
                let number = |ty: &Type| matches!(ty, Type::Int | Type::Float | Type::Unknown);
                match (op, &left_ty, &right_ty) {
                    (BinaryOp::Add | BinaryOp::Sub, Type::Duration, Type::Duration) => {
                        Ok(Type::Duration)
                    }
                    (BinaryOp::Mul, Type::Duration, right) if number(right) => Ok(Type::Duration),
                    (BinaryOp::Mul, left, Type::Duration) if number(left) => Ok(Type::Duration),
                    (BinaryOp::Div, Type::Duration, right) if number(right) => Ok(Type::Duration),
                    (BinaryOp::Div, Type::Duration, Type::Duration) => Ok(Type::Float),
                    (BinaryOp::Rem, Type::Duration, Type::Duration) => Ok(Type::Duration),
                    (_, Type::Unknown, _) | (_, _, Type::Unknown) => Ok(Type::Unknown),
                    _ => Err(RillError::new(format!(
                        "operator '{}' is not defined for {left_ty} and {right_ty}",
                        operator_symbol(op)
                    ))),
                }
            }
            BinaryOp::Add
            | BinaryOp::Sub
            | BinaryOp::Mul
            | BinaryOp::Div
            | BinaryOp::Rem
            | BinaryOp::Pow => {
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
            BinaryOp::Range | BinaryOp::RangeInclusive => {
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
        self.pop_scope();
        result
    }

    fn check_collection_callback(
        &self,
        name: &str,
        collection: &Type,
        callback: &Type,
    ) -> Result<(), RillError> {
        let arity = match collection {
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
            Type::List(_) | Type::Map(_) | Type::Option(_) | Type::Result(..) => {
                crate::methods::callback_arity(name, method_kind(collection))
            }
            _ => return Err(RillError::new(format!("{name} cannot iterate {collection}"))),
        };
        if let Type::Function { arity: Some(actual), .. } = callback {
            if *actual != arity {
                return Err(RillError::new(format!(
                    "{name} callback needs {arity} arg{}, got {actual}",
                    if arity == 1 { "" } else { "s" }
                )));
            }
            return Ok(());
        }
        if arity == 1 {
            self.check_callback(name, callback.clone())
        } else {
            Ok(())
        }
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
            Ok(Type::Function {
                required: None,
                names: None,
                arity: Some(1),
                params: None,
                return_type: Box::new(result_type),
            })
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
                    if !matches!(ty, Type::Map(_) | Type::Unknown) {
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
                    match part {
                        CommandPart::Var(name) => {
                            let ty = self.env.get(name).ok_or_else(|| {
                                RillError::spanned(
                                    format!("unknown command variable '${name}'"),
                                    span,
                                )
                            })?;
                            if matches!(ty, Type::Function { .. }) {
                                return Err(RillError::spanned(
                                    format!("cannot pass function '${name}' as argv"),
                                    span,
                                ));
                            }
                        }
                        CommandPart::VarDefault(name, default) => {
                            self.env.touch(name);
                            self.check_parts(default)?;
                        }
                        CommandPart::Expr(expr) => {
                            let ty =
                                self.check_expr(expr).map_err(|error| error.with_span(span))?;
                            if matches!(ty, Type::Function { .. }) {
                                return Err(RillError::spanned(
                                    "cannot pass a function as argv".to_string(),
                                    span,
                                ));
                            }
                        }
                        CommandPart::Text(_) => {}
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
    if let Type::Function { arity, params, return_type, .. } = ty {
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

/// Names a module exports: its top-level `let` bindings and `fn`s.  Mutable
/// `var`s stay private to the module.
pub(crate) fn top_level_names(stmts: &[Stmt]) -> Vec<String> {
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
        match stmt.unspanned() {
            Stmt::Let { pattern, mutable: false, .. } => pattern_names(pattern, &mut names),
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
        Value::Map(_) => Type::map_any(),
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
            required: Some(function.function.params.iter().filter(|p| p.default.is_none()).count()),
            names: Some(function.function.params.iter().map(|p| p.name.clone()).collect()),
            arity: Some(function.function.params.len()),
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
        Type::Map(_) => Kind::Map,
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
        Return::Map => Type::map_any(),
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
        Return::MapEntries => {
            let value = match receiver {
                Type::Map(value) => (**value).clone(),
                _ => Type::Unknown,
            };
            Type::List(Box::new(Type::Tuple(vec![Type::Str, value])))
        }
        Return::MapValues => Type::List(Box::new(match receiver {
            Type::Map(value) => (**value).clone(),
            _ => Type::Unknown,
        })),
        Return::Mapped => {
            let produced = args.first().map(function_return).unwrap_or(Type::Unknown);
            match receiver {
                Type::Option(_) => Type::Option(Box::new(produced)),
                Type::Result(_, error) => Type::Result(Box::new(produced), error.clone()),
                Type::List(_) | Type::Map(_) => Type::List(Box::new(produced)),
                _ => Type::Unknown,
            }
        }
        Return::MappedErr => match receiver {
            Type::Result(value, _) => Type::Result(
                value.clone(),
                Box::new(args.first().map(function_return).unwrap_or(Type::Unknown)),
            ),
            _ => Type::Unknown,
        },
        Return::AndThen => args.first().map(function_return).unwrap_or(Type::Unknown),
        Return::OkOrResult => match receiver {
            Type::Option(value) => Type::Result(
                value.clone(),
                Box::new(args.first().cloned().unwrap_or(Type::Unknown)),
            ),
            _ => Type::Unknown,
        },
        Return::GetResult => match receiver {
            Type::List(item) => Type::Option(item.clone()),
            _ => Type::Unknown,
        },
        Return::ZipPairs => Type::List(Box::new(Type::Tuple(vec![
            receiver.list_item(),
            args.first().map_or(Type::Unknown, Type::list_item),
        ]))),
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
        TypeName::Map(value) => {
            Type::Map(Box::new(value.as_deref().map(type_from_name).unwrap_or(Type::Unknown)))
        }
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
            required: None,
            names: None,
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
        (Type::Map(expected), Type::Map(actual)) => ensure_assignable(expected, actual),
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
                ..
            },
            Type::Function {
                arity: actual_arity,
                params: actual_params,
                return_type: actual_rt,
                ..
            },
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
    ensure_arity_range(name, expected, expected, actual)
}

fn ensure_arity_range(
    name: &str,
    required: usize,
    maximum: usize,
    actual: usize,
) -> Result<(), RillError> {
    if (required..=maximum).contains(&actual) {
        return Ok(());
    }
    let expected =
        if required == maximum { required.to_string() } else { format!("{required} to {maximum}") };
    Err(RillError::new(format!("{name} needs {expected} args, got {actual}")))
}

/// Positional arguments and `name = value` pairs of a call.
type SplitArgs<'a> = (Vec<&'a Expr>, Vec<(&'a str, &'a Expr)>);

/// Split call arguments into positional and named parts, checking order.
fn split_call_args(args: &[Expr]) -> Result<SplitArgs<'_>, RillError> {
    let mut positional = Vec::new();
    let mut named = Vec::new();
    for arg in args {
        match arg.unspanned() {
            Expr::NamedArg { name, value } => named.push((name.as_str(), &**value)),
            _ => {
                if !named.is_empty() {
                    return Err(RillError::new(
                        "positional arguments must come before named arguments",
                    ));
                }
                positional.push(arg);
            }
        }
    }
    Ok((positional, named))
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
        Expr::Slice { base, start, end, .. } => {
            contains_implicit(base)
                || start.as_deref().is_some_and(contains_implicit)
                || end.as_deref().is_some_and(contains_implicit)
        }
        Expr::Binary { left, right, .. } => contains_implicit(left) || contains_implicit(right),
        Expr::Pipe { left, right, .. } => contains_implicit(left) || contains_implicit(right),
        Expr::RegexMatch { value, .. }
        | Expr::RegexSubstitution { value, .. }
        | Expr::Match { value, .. } => contains_implicit(value),
        Expr::NamedArg { value, .. } => contains_implicit(value),
        Expr::Try(value) => contains_implicit(value),
        Expr::For { iter, .. } => contains_implicit(iter),
        Expr::Block(stmts) => stmts.iter().any(|stmt| match stmt.unspanned() {
            Stmt::Let { expr, .. } | Stmt::Expr(expr) | Stmt::Emit(expr) => contains_implicit(expr),
            Stmt::Assign { target, expr, .. } => {
                contains_implicit(target) || contains_implicit(expr)
            }
            Stmt::While { cond, .. } => contains_implicit(cond),
            Stmt::On { host, .. } => contains_implicit(host),
            Stmt::Return(Some(expr)) => contains_implicit(expr),
            _ => false,
        }),
        Expr::Unary { operand, .. } => contains_implicit(operand),
        Expr::Closure { .. } | Expr::Literal(_) | Expr::Ident(_) | Expr::Command(_) => false,
        Expr::Located { expr, .. } => contains_implicit(expr),
        Expr::Interpolated(parts) => parts.iter().any(|part| match part {
            StrPart::Expr(expr) => contains_implicit(expr),
            _ => false,
        }),
        Expr::If { cond, .. } => contains_implicit(cond),
        Expr::Loop { .. } => false,
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
        (Type::Map(left), Type::Map(right)) => Type::Map(Box::new(common_type(*left, *right))),
        (Type::Option(left), Type::Option(right)) => {
            Type::Option(Box::new(common_type(*left, *right)))
        }
        (Type::Result(left, left_error), Type::Result(right, right_error)) => Type::Result(
            Box::new(common_type(*left, *right)),
            Box::new(common_type(*left_error, *right_error)),
        ),
        (Type::Tuple(left), Type::Tuple(right)) if left.len() == right.len() => Type::Tuple(
            left.into_iter().zip(right).map(|(left, right)| common_type(left, right)).collect(),
        ),
        (
            Type::Function { arity: left_arity, return_type: left_return, .. },
            Type::Function { arity: right_arity, return_type: right_return, .. },
        ) if left_arity == right_arity => Type::Function {
            arity: left_arity,
            required: None,
            params: None,
            names: None,
            return_type: Box::new(common_type(*left_return, *right_return)),
        },
        _ => Type::Unknown,
    }
}

fn constant_int(expr: &Expr) -> Option<i64> {
    match expr.unspanned() {
        Expr::Literal(Literal::Int(value)) => Some(*value),
        Expr::Unary { op: UnaryOp::Neg, operand } => match operand.unspanned() {
            Expr::Literal(Literal::Int(value)) => value.checked_neg(),
            _ => None,
        },
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
        (AssignOp::Add, Type::List(current), Type::List(value)) => {
            Ok(Type::List(Box::new(common_type((**current).clone(), (**value).clone()))))
        }
        (
            AssignOp::Add | AssignOp::Sub | AssignOp::Mul | AssignOp::Div | AssignOp::Rem,
            Type::Int | Type::Float,
            Type::Int | Type::Float,
        ) => Ok(common_type(current.clone(), value.clone())),
        (AssignOp::Add | AssignOp::Sub, Type::Duration, Type::Duration) => Ok(Type::Duration),
        (AssignOp::Mul | AssignOp::Div, Type::Duration, Type::Int | Type::Float) => {
            Ok(Type::Duration)
        }
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

fn operator_symbol(op: BinaryOp) -> &'static str {
    match op {
        BinaryOp::Add => "+",
        BinaryOp::Sub => "-",
        BinaryOp::Mul => "*",
        BinaryOp::Div => "/",
        BinaryOp::Rem => "%",
        BinaryOp::Pow => "**",
        BinaryOp::Gt => ">",
        BinaryOp::Ge => ">=",
        BinaryOp::Lt => "<",
        BinaryOp::Le => "<=",
        BinaryOp::Eq => "==",
        BinaryOp::Ne => "!=",
        BinaryOp::Range => "..",
        BinaryOp::RangeInclusive => "..=",
        BinaryOp::And => "&&",
        BinaryOp::Or => "||",
        BinaryOp::In => "in",
        BinaryOp::NotIn => "not in",
        BinaryOp::Coalesce => "??",
    }
}

/// Which cases unguarded match arms have covered, for exhaustiveness.
#[derive(Default)]
struct Coverage {
    irrefutable: bool,
    saw_true: bool,
    saw_false: bool,
    saw_null: bool,
    saw_none: bool,
    saw_some: bool,
    saw_ok: bool,
    saw_err: bool,
}

impl Coverage {
    fn add(&mut self, pattern: &Pattern) {
        match pattern {
            _ if pattern.is_irrefutable() => self.irrefutable = true,
            Pattern::Literal(Literal::Bool(true)) => self.saw_true = true,
            Pattern::Literal(Literal::Bool(false)) => self.saw_false = true,
            Pattern::Literal(Literal::Null) => self.saw_null = true,
            Pattern::None => self.saw_none = true,
            Pattern::Some(inner) if inner.is_irrefutable() => self.saw_some = true,
            Pattern::Ok(inner) if inner.is_irrefutable() => self.saw_ok = true,
            Pattern::Err(inner) if inner.is_irrefutable() => self.saw_err = true,
            Pattern::Or(alternatives) => {
                for alternative in alternatives {
                    self.add(alternative);
                }
            }
            _ => {}
        }
    }

    fn complete(&self, value_ty: &Type) -> bool {
        self.irrefutable
            || match value_ty {
                Type::Bool => self.saw_true && self.saw_false,
                Type::Null => self.saw_null,
                Type::Option(_) => self.saw_none && self.saw_some,
                Type::Result(..) => self.saw_ok && self.saw_err,
                _ => false,
            }
    }
}

/// Parameter types for the callback of `method` on `receiver`, used to type
/// closure parameters and the implicit `.` argument.
fn callback_param_types(method: &str, receiver: &Type) -> Vec<Type> {
    match (method, receiver) {
        (_, Type::List(item)) => vec![(**item).clone()],
        ("map" | "parallel_map", Type::Map(value)) => vec![Type::Str, (**value).clone()],
        ("map" | "and_then", Type::Option(inner)) => vec![(**inner).clone()],
        ("map" | "and_then", Type::Result(value, _)) => vec![(**value).clone()],
        ("map_err" | "unwrap_or_else", Type::Result(_, error)) => vec![(**error).clone()],
        ("unwrap_or_else", Type::Option(_)) => Vec::new(),
        _ => Vec::new(),
    }
}

/// The variable at the root of an assignment target.
fn place_root(place: &Expr) -> Option<&str> {
    match place.unspanned() {
        Expr::Ident(name) => Some(name),
        Expr::Field { base, .. } | Expr::Index { base, .. } => place_root(base),
        _ => None,
    }
}

fn unknown_namespace_function(module: &str, name: &str) -> RillError {
    RillError::new(format!("{module} has no function '{name}'")).with_optional_help(
        crate::suggest::did_you_mean(name, crate::methods::namespace_functions(module)),
    )
}

/// Error for `value.name` when `value`'s type has no such field or method.
fn unknown_member(ty: &Type, name: &str) -> RillError {
    let help = if let Some(renamed) = crate::methods::moved_function(name) {
        Some(format!("`.{name}()` was renamed; use `.{renamed}()`"))
    } else {
        crate::suggest::closest(name, crate::methods::methods_for(method_kind(ty)))
            .map(|candidate| format!("did you mean `.{candidate}()`?"))
    };
    RillError::new(format!("{ty} has no field '{name}'")).with_optional_help(help)
}

/// Decide what an unannotated propagating body returns, record it on the AST
/// for the evaluator, and produce the type callers see.
fn infer_propagation(
    cell: &PropagationCell,
    annotated: bool,
    tries: &[TryKind],
    body_ty: Type,
) -> Type {
    if annotated || tries.is_empty() {
        cell.set(Propagation::None);
        return body_ty;
    }
    let all_option = tries.iter().all(|kind| *kind == TryKind::Option);
    if all_option {
        cell.set(Propagation::Option);
        match body_ty {
            Type::Option(_) | Type::Result(..) => body_ty,
            other => Type::Option(Box::new(other)),
        }
    } else {
        cell.set(Propagation::Result);
        match body_ty {
            Type::Option(_) | Type::Result(..) => body_ty,
            other => Type::Result(Box::new(other), Box::new(Type::Unknown)),
        }
    }
}

/// The keyword that makes a statement leave its block unconditionally, if any.
fn terminator(stmt: &Stmt) -> Option<&'static str> {
    match stmt.unspanned() {
        Stmt::Return(_) => Some("return"),
        Stmt::Break(_) => Some("break"),
        Stmt::Continue => Some("continue"),
        Stmt::Expr(expr) => match expr.unspanned() {
            Expr::Call { callee, .. } => match callee.unspanned() {
                Expr::Ident(name) if name == "die" => Some("die"),
                Expr::Ident(name) if name == "exit" => Some("exit"),
                _ => None,
            },
            _ => None,
        },
        _ => None,
    }
}

/// Whether every path through `stmts` ends by producing a value: a `return`,
/// a final expression, an `if`/`else` or `match` whose branches all do, or a
/// call that never returns.
fn stmts_always_yield(stmts: &[Stmt]) -> bool {
    let Some(last) = stmts.last() else {
        return false;
    };
    // A `return` (or a call that never returns) anywhere in the block ends it.
    if stmts.iter().any(|stmt| matches!(terminator(stmt), Some("return" | "die" | "exit"))) {
        return true;
    }
    match last.unspanned() {
        Stmt::Return(_) => true,
        Stmt::Expr(expr) => expr_always_yields(expr),
        Stmt::Located { stmt, .. } => stmts_always_yield(std::slice::from_ref(stmt)),
        _ => false,
    }
}

fn expr_always_yields(expr: &Expr) -> bool {
    match expr.unspanned() {
        Expr::Match { arms, .. } => arms.iter().all(|arm| expr_always_yields(&arm.expr)),
        Expr::Block(body) => stmts_always_yield(body),
        Expr::If { then_body, else_body, .. } => {
            !else_body.is_empty() && stmts_always_yield(then_body) && stmts_always_yield(else_body)
        }
        Expr::Loop { .. } => true,
        Expr::Call { callee, .. } => {
            !matches!(callee.unspanned(), Expr::Ident(name) if name == "print" || name == "warn")
        }
        _ => true,
    }
}
