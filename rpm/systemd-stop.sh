#!/bin/bash

set -eu

systemctl stop rezolus-exporter
systemctl disable rezolus-exporter

systemctl stop rezolus
systemctl disable rezolus
