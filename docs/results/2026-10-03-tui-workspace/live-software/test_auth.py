import time
import unittest
from auth import authorize

class AuthorizationTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        print("AUTH_CHECK_RUNNING", flush=True)
        time.sleep(12)

    def test_expired(self): self.assertEqual(authorize({"subject":"alice","expires_at":9},10)[0],401)
    def test_boundary(self): self.assertEqual(authorize({"subject":"alice","expires_at":10},10)[0],401)
    def test_valid(self): self.assertEqual(authorize({"subject":"alice","expires_at":11},10),(200,"alice"))
    def test_future(self): self.assertEqual(authorize({"subject":"bob","expires_at":1000},10),(200,"bob"))
    def test_none(self): self.assertEqual(authorize(None,10)[0],401)
    def test_not_dict(self): self.assertEqual(authorize("bad",10)[0],401)
    def test_empty_subject(self): self.assertEqual(authorize({"subject":"","expires_at":11},10)[0],401)
    def test_missing_expiry(self): self.assertEqual(authorize({"subject":"alice"},10)[0],401)
    def test_string_expiry(self): self.assertEqual(authorize({"subject":"alice","expires_at":"11"},10)[0],401)
    def test_boolean_expiry(self): self.assertEqual(authorize({"subject":"alice","expires_at":True},10)[0],401)
    def test_negative_expiry(self): self.assertEqual(authorize({"subject":"alice","expires_at":-1},10)[0],401)
    def test_fractional_expiry(self): self.assertEqual(authorize({"subject":"alice","expires_at":10.5},10),(200,"alice"))
