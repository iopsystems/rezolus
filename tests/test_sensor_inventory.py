"""Regression checks for portable sensor inventory values."""
import importlib.util
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location(
    "sensor_inventory", Path(__file__).resolve().parents[1] / "scripts/sensor_inventory.py"
)
sensor_inventory = importlib.util.module_from_spec(spec)
spec.loader.exec_module(sensor_inventory)


class InventoryTests(unittest.TestCase):
    def test_unreadable_value_is_null(self):
        with tempfile.TemporaryDirectory() as root:
            missing = Path(root) / "model"
            errors = {}
            self.assertIsNone(sensor_inventory.read(missing, errors))
            self.assertIn(str(missing), errors)

    def test_device_tree_strings_keep_their_values(self):
        with tempfile.TemporaryDirectory() as root:
            compatible = Path(root) / "compatible"
            compatible.write_bytes(b"nvidia,board\0nvidia,tegra264\0")
            self.assertEqual(sensor_inventory.read(compatible), "nvidia,board\nnvidia,tegra264")


if __name__ == "__main__":
    unittest.main()
