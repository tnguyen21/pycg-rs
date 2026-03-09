"""Golden accuracy fixture: factory function return flow.

Covers:
- factory() constructs a Product and returns it: uses edge factory -> Product is emitted
- consumer() calls factory() then calls .make() on the returned object

Known gap: result.make() is NOT currently resolved because the return value
is opaque (single-value binding not propagated through call sites).  The gap
is documented in test_accuracy_factory_return_method_gap (marked #[ignore]).

Adapted from PyCG micro-benchmark returns/call, classes/return_call.
"""


class Product:
    def make(self):
        pass


def factory():
    """Returns a Product instance."""
    return Product()


def consumer():
    """Calls factory then calls .make() on the returned object.

    Currently: factory is tracked as used.
    Currently: 'make' is NOT tracked (opaque return-value gap).
    Ideal: consumer -> Product.make via return-value type propagation.
    """
    result = factory()
    result.make()
