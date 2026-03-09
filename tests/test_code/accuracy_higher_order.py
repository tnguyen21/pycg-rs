"""Golden accuracy fixture: higher-order function flows.

Covers:
- make_greeter() returns a closure (inner function); make_greeter() is a factory
  returning a callable, so make_greeter()() should be resolvable.
- get_adder() returns add_nums; calling the returned function should resolve.

Adapted from PyCG micro-benchmark functions/assigned_call, returns/return_call.
"""


def greet():
    pass


def make_greeter():
    """Returns the greet function."""
    return greet


def call_via_factory():
    """Calls make_greeter()() — a higher-order call through a factory return."""
    fn = make_greeter()
    fn()


# ---

def add_nums(x, y):
    pass


def get_adder():
    """Returns add_nums callable."""
    return add_nums


def call_returned_fn():
    """Assigns result of get_adder() and calls it."""
    adder = get_adder()
    adder(1, 2)
