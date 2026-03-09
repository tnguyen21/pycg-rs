# Accuracy fixture: branch-join / rebinding preserves all pointees (INV-1)
#
# After:
#   if cond: x = A()
#   else:    x = B()
#   x.method()
#
# The analyzer must record uses edges to BOTH A.method and B.method
# because x can point to either A or B.

class A:
    def method(self):
        pass

class B:
    def method(self):
        pass

def caller(cond):
    if cond:
        x = A()
    else:
        x = B()
    x.method()


# Simple rebinding: after x = A() then x = B(), x -> {A, B}
def rebind_caller():
    x = A()
    x = B()
    x.method()
