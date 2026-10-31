import math
import unittest

from janus_adapter import frames


def rot_y(a):
    c, s = math.cos(a), math.sin(a)
    return (c, 0.0, s, 0.0, 1.0, 0.0, -s, 0.0, c)


def rot_z(a):
    c, s = math.cos(a), math.sin(a)
    return (c, -s, 0.0, s, c, 0.0, 0.0, 0.0, 1.0)


class FrameTests(unittest.TestCase):
    def assertClose(self, a, b, tol=1e-12):
        self.assertEqual(len(a), len(b))
        for x, y in zip(a, b):
            self.assertAlmostEqual(x, y, delta=tol)

    def test_enu_is_identity(self):
        self.assertEqual(frames.vec_to_enu((1, 2, 3), "ENU"), (1, 2, 3))
        self.assertEqual(frames.vec_from_enu((1, 2, 3), "ENU"), (1, 2, 3))

    def test_nue_vectors(self):
        # NUE: x=North, y=Up, z=East  ->  ENU = (East, North, Up) = (z, x, y)
        self.assertEqual(frames.vec_to_enu((10.0, 20.0, 30.0), "NUE"), (30.0, 10.0, 20.0))
        v = (1.5, -2.0, 7.0)
        self.assertEqual(frames.vec_from_enu(frames.vec_to_enu(v, "NUE"), "NUE"), v)

    def test_up_is_z_in_enu(self):
        self.assertEqual(frames.vec_to_enu((0.0, 1.0, 0.0), "NUE"), (0.0, 0.0, 1.0))

    def test_rotation_about_up_becomes_rotation_about_z(self):
        a = 0.7
        self.assertClose(frames.matrix_to_enu(rot_y(a), "NUE"), rot_z(a))

    def test_handedness_preserved(self):
        m = frames.matrix_to_enu(rot_y(0.3), "NUE")
        det = (m[0] * (m[4] * m[8] - m[5] * m[7]) - m[1] * (m[3] * m[8] - m[5] * m[6]) + m[2] * (m[3] * m[7] - m[4] * m[6]))
        self.assertAlmostEqual(det, 1.0)

    def test_quaternions(self):
        self.assertClose(frames.matrix_to_quat(rot_z(0.0)), (0.0, 0.0, 0.0, 1.0))
        q = frames.matrix_to_quat(rot_z(math.pi / 2))
        self.assertClose(q, (0.0, 0.0, math.sin(math.pi / 4), math.cos(math.pi / 4)))
        q180 = frames.matrix_to_quat(rot_z(math.pi))  # exercises the non-trace branch
        self.assertClose(q180, (0.0, 0.0, 1.0, 0.0))
        for ax in range(3):  # each principal-axis 180 deg flip
            m = [[1.0 if i == j else -1.0 for j in range(3)] for i in range(3)]
            m[ax][ax] = 1.0
            q = frames.matrix_to_quat([x for row in m for x in row])
            self.assertAlmostEqual(sum(c * c for c in q), 1.0)

    def test_eun_rejected(self):
        with self.assertRaises(ValueError):
            frames.check_supported("EUN")
        frames.check_supported("ENU")
        frames.check_supported("NUE")


if __name__ == "__main__":
    unittest.main()
