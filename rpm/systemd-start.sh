#!/bin/bash

set -eu

systemctl enable rezolus
systemctl start rezolus

systemctl enable rezolus-exporter
systemctl start rezolus-exporter
