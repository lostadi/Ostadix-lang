# Example external Python script invoked via run_script()
# This file is loaded and executed by O-lang instead of being
# embedded inline inside python^(...)_python.

numbers = [1, 2, 3, 4, 5]
total = sum(numbers)
__oval_result__ = f"Sum of {numbers} = {total}"
