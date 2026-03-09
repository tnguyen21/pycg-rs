"""Golden accuracy fixture: decorator uses-edge flow.

Covers:
- @simple_decorator applied to a function creates a module-level uses edge to the decorator
- @factory_decorator("arg") (parameterised decorator) also creates a module-level uses edge
- A caller function that calls decorated functions emits uses edges to those functions

Adapted from PyCG micro-benchmark decorators/call, decorators/param_call.
"""


def simple_decorator(f):
    return f


def factory_decorator(param):
    def decorator(f):
        return f
    return decorator


@simple_decorator
def simple_decorated():
    pass


@factory_decorator("arg")
def factory_decorated():
    pass


def call_decorated():
    """Calls both decorated functions — must produce uses edges to each."""
    simple_decorated()
    factory_decorated()
