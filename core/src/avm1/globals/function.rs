//! Function prototype

use crate::avm1::error::Error;
use crate::avm1::function::{ExecutionName, ExecutionReason};
use crate::avm1::property_decl::{define_properties_on, Declaration};
use crate::avm1::{activation::Activation, AvmString};
use crate::avm1::{Object, ScriptObject, TObject, Value};
use gc_arena::MutationContext;

const PROTO_DECLS: &[Declaration] = declare_properties! {
    "call" => method(call);
    "apply" => method(apply);
    "toString" => method(to_string);
};

/// Implements `new Function()`
pub fn constructor<'gc>(
    _activation: &mut Activation<'_, 'gc, '_>,
    this: Object<'gc>,
    _args: &[Value<'gc>],
) -> Result<Value<'gc>, Error<'gc>> {
    Ok(this.into())
}

/// Implements `Function()`
pub fn function<'gc>(
    activation: &mut Activation<'_, 'gc, '_>,
    _this: Object<'gc>,
    args: &[Value<'gc>],
) -> Result<Value<'gc>, Error<'gc>> {
    if let Some(arg) = args.get(0) {
        Ok(arg.to_owned())
    } else {
        // Calling `Function()` seems to give a prototypeless bare object.
        Ok(ScriptObject::object(activation.context.gc_context, None).into())
    }
}

/// Implements `Function.prototype.call`
pub fn call<'gc>(
    activation: &mut Activation<'_, 'gc, '_>,
    func: Object<'gc>,
    myargs: &[Value<'gc>],
) -> Result<Value<'gc>, Error<'gc>> {
    let this = match myargs.get(0).unwrap_or(&Value::Undefined) {
        Value::Undefined | Value::Null => activation.context.avm1.globals,
        this_val => this_val.coerce_to_object(activation),
    };
    let empty = [];
    let args = match myargs.len() {
        0 => &empty,
        1 => &empty,
        _ => &myargs[1..],
    };

    match func.as_executable() {
        Some(exec) => exec.exec(
            ExecutionName::Static("[Anonymous]"),
            activation,
            this,
            1,
            args,
            ExecutionReason::FunctionCall,
            func,
        ),
        _ => Ok(Value::Undefined),
    }
}

/// Implements `Function.prototype.apply`
pub fn apply<'gc>(
    activation: &mut Activation<'_, 'gc, '_>,
    func: Object<'gc>,
    myargs: &[Value<'gc>],
) -> Result<Value<'gc>, Error<'gc>> {
    let this = match myargs.get(0).unwrap_or(&Value::Undefined) {
        Value::Undefined | Value::Null => activation.context.avm1.globals,
        this_val => this_val.coerce_to_object(activation),
    };
    let args_object = myargs.get(1).cloned().unwrap_or(Value::Undefined);
    let length = match args_object {
        Value::Object(a) => a.get("length", activation)?.coerce_to_f64(activation)? as usize,
        _ => 0,
    };

    let mut child_args = Vec::with_capacity(length);
    while child_args.len() < length {
        let args = args_object.coerce_to_object(activation);
        // TODO: why don't this use args_object.array_element?
        let next_arg = format!("{}", child_args.len());
        let next_arg = args.get(
            AvmString::new_utf8(activation.context.gc_context, next_arg),
            activation,
        )?;

        child_args.push(next_arg);
    }

    match func.as_executable() {
        Some(exec) => exec.exec(
            ExecutionName::Static("[Anonymous]"),
            activation,
            this,
            1,
            &child_args,
            ExecutionReason::FunctionCall,
            func,
        ),
        _ => Ok(Value::Undefined),
    }
}

/// Implements `Function.prototype.toString`
fn to_string<'gc>(
    _: &mut Activation<'_, 'gc, '_>,
    _: Object<'gc>,
    _: &[Value<'gc>],
) -> Result<Value<'gc>, Error<'gc>> {
    Ok("[type Function]".into())
}

/// Partially construct `Function.prototype`.
///
/// `__proto__` and other cross-linked properties of this object will *not*
/// be defined here. The caller of this function is responsible for linking
/// them in order to obtain a valid ECMAScript `Function` prototype. The
/// returned object is also a bare object, which will need to be linked into
/// the prototype of `Object`.
pub fn create_proto<'gc>(gc_context: MutationContext<'gc, '_>, proto: Object<'gc>) -> Object<'gc> {
    let function_proto = ScriptObject::object_cell(gc_context, Some(proto));
    let object = function_proto.as_script_object().unwrap();
    define_properties_on(PROTO_DECLS, gc_context, object, function_proto);
    function_proto
}
