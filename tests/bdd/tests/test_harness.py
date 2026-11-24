"""Unit tests for BDD harness labels, image input hashing, and artifact generation."""

from __future__ import annotations

import tempfile
import unittest
from pathlib import Path

from updater_plus_bdd.config import BDD_LABEL_KEY, BDD_SCENARIO_LABEL_KEY
from updater_plus_bdd.drivers.artifacts import ArtifactFactory
from updater_plus_bdd.images import ImageBuilder
from updater_plus_bdd.stack import bdd_labels


class HarnessHelpersTest(unittest.TestCase):
    def test_bdd_labels_slugify_scenario_name(self) -> None:
        labels = bdd_labels("Sequential updates over Nginx (206 multipart/byteranges)!")
        self.assertEqual(labels[BDD_LABEL_KEY], "1")
        self.assertEqual(
            labels[BDD_SCENARIO_LABEL_KEY],
            "sequential-updates-over-nginx-206-multip",
        )
        self.assertEqual(bdd_labels("!!!")[BDD_SCENARIO_LABEL_KEY], "scenario")
        self.assertEqual(bdd_labels("")[BDD_SCENARIO_LABEL_KEY], "scenario")

    def test_image_builder_hashes_service_inputs_deterministically(self) -> None:
        builder = ImageBuilder()
        h1 = builder.compute_build_hash("nginx-release")
        h2 = builder.compute_build_hash("nginx-release")
        h3 = builder.compute_build_hash("cdn-gateway")
        self.assertEqual(len(h1), 24)
        self.assertEqual(h1, h2)
        self.assertNotEqual(h1, h3)
        with self.assertRaises(ValueError):
            builder.ensure("unknown-service")

    def test_artifact_factory_produces_mostly_identical_deltas_and_bundle_symlinks(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            factory = ArtifactFactory(Path(tmp))
            with self.assertRaises(KeyError):
                factory.path_for("9.9.9")

            v1 = factory.build_base("0.1.0", size_bytes=200_000)
            v2 = factory.build_delta_from(
                "0.1.0",
                "0.2.0",
                mutated_offsets=(50_000,),
                span_bytes=1024,
            )
            b1 = v1.read_bytes()
            b2 = v2.read_bytes()
            self.assertEqual(len(b1), 200_000)
            self.assertEqual(len(b2), 200_000)
            self.assertNotEqual(b1[50_000:51_024], b2[50_000:51_024])
            self.assertEqual(b1[49_000:50_000], b2[49_000:50_000])
            self.assertEqual(b1[51_024:52_000], b2[51_024:52_000])
            self.assertEqual(b1[100_000:150_000], b2[100_000:150_000])

            bundle = factory.create_macos_bundle("Demo.app", "0.2.0")
            fw_link = bundle / "Contents" / "Frameworks" / "Core.framework" / "Core"
            self.assertTrue(fw_link.is_symlink())
            self.assertEqual(fw_link.readlink(), Path("Versions/A/Core"))


if __name__ == "__main__":
    unittest.main()
