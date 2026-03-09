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


# Conditional rebinding: x = A() unconditionally; then x = B() only when flag
# is True.  Both A.method and B.method are genuinely reachable depending on the
# runtime value of flag, so a sound analysis must retain both in the join.
def conditional_rebind_caller(flag):
    x = A()
    if flag:
        x = B()
    x.method()
