"""Golden accuracy fixture: variable aliasing and rebinding.

Covers:
- simple alias: a = func; a() must produce a uses edge to func
- chained assignment: a = b = func1 then a = b = func2 must track both funcs
- value-set rebinding: alias = func_a; alias = func_b — both must be retained (INV-2)

Adapted from PyCG micro-benchmark assignments/chained, functions/assigned_call.
"""


def target_one():
    pass


def target_two():
    pass


def simple_alias_caller():
    """Assigning a function to a variable and calling via alias."""
    a = target_one
    a()


def chained_alias_caller():
    """Chained assignment a = b = target_one then rebind a = b = target_two."""
    a = b = target_one
    b()
    a = b = target_two
    a()


# ValueSet invariant fixtures (INV-2):
# After `alias = func_a; alias = func_b`, alias -> {func_a, func_b}
# so calling alias() must emit uses edges to both.

def func_a():
    pass


def func_b():
    pass


def alias_caller():
    alias = func_a
    alias = func_b
    alias()


def bar():
    pass


def import_alias_caller():
    foo = func_a
    foo = bar
    foo()
