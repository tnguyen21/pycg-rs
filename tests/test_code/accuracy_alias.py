"""Golden accuracy fixture: variable aliasing and rebinding.

Covers:
- simple alias: a = func; a() must produce a uses edge to func
- chained assignment: a = b = func1 then a = b = func2 must track both funcs

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
