/**
 * ColorMapper provides consistent color assignment for cgroups across all charts and page refreshes
 * using a deterministic hash function to assign colors based on cgroup names.
 *
 * Can also be configured (useConsistentCgroupColors == false) to use a more distinguishable (but inconsistent)
 * color palette. This assigns colors to each series based on its index rather than its name.
 *
 * In all cases, the "Other" category is always a muted gray.
 */
export class ColorMapper {
    constructor() {
        // Store color assignments for cgroups
        this.colorMap = new Map();
        // Track which cgroups are selected (and thus need colors)
        this.selectedCgroups = new Set();
        // Track the order colors were assigned
        this.colorAssignmentOrder = [];

        // Curated color palette optimized for dark backgrounds
        // Designed for maximum distinguishability while maintaining visual harmony
        this.colorPalette = [
            '#58a6ff', // Electric blue (primary)
            '#39d5ff', // Bright cyan
            '#2dd4bf', // Teal
            '#3fb950', // Green
            '#a3e635', // Lime
            '#fbbf24', // Amber
            '#f97316', // Orange
            '#f85149', // Red
            '#f472b6', // Pink
            '#a78bfa', // Purple
            '#818cf8', // Indigo
            '#38bdf8', // Sky blue
            '#34d399', // Emerald
            '#facc15', // Yellow
            '#fb923c', // Light orange
            '#e879f9', // Fuchsia
            '#c084fc', // Violet
            '#22d3ee', // Cyan bright
            '#4ade80', // Light green
            '#fca5a1', // Light coral
        ];

        // Muted gray for "Other" category - consistent across all charts
        this.otherColor = '#484f58';
    }

    /**
     * Get a simple hash value from a string
     * This creates a deterministic numeric value from any string
     * @param {string} str - The string to hash
     * @returns {number} A numeric hash value
     */
    stringToHash(str) {
        let hash = 0;
        for (let i = 0; i < str.length; i++) {
            const char = str.charCodeAt(i);
            hash = ((hash << 5) - hash) + char;
            hash = hash & hash; // Convert to 32bit integer
        }
        // Make sure it's positive
        return Math.abs(hash);
    }

    /**
     * Get the color for a specific cgroup, using a deterministic mapping based on the cgroup name.
     * Special case for "Other" category
     * @param {string} cgroupName - The name of the cgroup
     * @returns {string} The color code for this cgroup
     */
    getColorByName(cgroupName) {
        // Special case for "Other" category
        if (cgroupName === "Other") {
            return this.otherColor;
        }

        // If we already have a color for this cgroup, return it
        if (this.colorMap.has(cgroupName)) {
            return this.colorMap.get(cgroupName);
        }

        // Generate a deterministic index based on the cgroup name
        const hash = this.stringToHash(cgroupName);
        const colorIndex = hash % this.colorPalette.length;
        const color = this.colorPalette[colorIndex];

        // Store the mapping for future reference
        this.colorMap.set(cgroupName, color);

        return color;
    }

    /**
     * Get the nth color in the fixed color palette.
     *
     * @param {number} index - The index to get a color for
     * @returns {string} The color code for this index
     */
    getColorByIndex(index) {
        // Use a jump pattern to maximize visual distinction between adjacent series
        const STARTING_INDEX = 0;
        const JUMP_DISTANCE = 7; // Prime number for better distribution
        return this.colorPalette[(STARTING_INDEX + index * JUMP_DISTANCE) % this.colorPalette.length];
    }

    /**
     * Get colors for an array of cgroup names
     * @param {string[]} cgroupNames - Array of cgroup names
     * @returns {string[]} Array of color codes in the same order
     */
    getColors(cgroupNames) {
        return cgroupNames.map((name) => this.getColorByName(name));
    }

    /**
     * Get color for the "Other" category
     * @returns {string} The color for the "Other" category
     */
    getOtherColor() {
        return this.otherColor;
    }

    /**
     * Clear all color mappings - generally only needed for testing
     */
    clear() {
        this.colorMap.clear();
        this.selectedCgroups.clear();
        this.colorAssignmentOrder = [];
    }

    /**
     * Add a cgroup to the selected set and assign it a color
     * @param {string} cgroupName - The name of the cgroup to select
     */
    selectCgroup(cgroupName) {
        if (!this.selectedCgroups.has(cgroupName) && cgroupName !== 'Other') {
            this.selectedCgroups.add(cgroupName);

            // Assign a color based on the order of selection
            if (!this.colorMap.has(cgroupName)) {
                const nextIndex = this.colorAssignmentOrder.length;
                // Use a more distinguishable color assignment based on selection order
                const color = this.getColorByIndex(nextIndex);
                this.colorMap.set(cgroupName, color);
                this.colorAssignmentOrder.push(cgroupName);
            }
        }
    }

    /**
     * Remove a cgroup from the selected set and free its color
     * @param {string} cgroupName - The name of the cgroup to deselect
     */
    deselectCgroup(cgroupName) {
        if (this.selectedCgroups.has(cgroupName)) {
            this.selectedCgroups.delete(cgroupName);

            // Remove from color assignment order
            const index = this.colorAssignmentOrder.indexOf(cgroupName);
            if (index > -1) {
                this.colorAssignmentOrder.splice(index, 1);
            }

            // Remove color mapping
            this.colorMap.delete(cgroupName);

            // Reassign colors to remaining selected cgroups to maintain order
            this.reassignColors();
        }
    }

    /**
     * Reassign colors to all selected cgroups based on their order
     */
    reassignColors() {
        this.colorMap.clear();
        this.colorAssignmentOrder.forEach((cgroupName, index) => {
            const color = this.getColorByIndex(index);
            this.colorMap.set(cgroupName, color);
        });
    }

    /**
     * Get the color for a selected cgroup
     * @param {string} cgroupName - The name of the cgroup
     * @returns {string|null} The color code for this cgroup, or null if not selected
     */
    getSelectedCgroupColor(cgroupName) {
        // Special case for "Other" category
        if (cgroupName === 'Other') {
            return this.otherColor;
        }

        // Only return a color if this cgroup is selected
        if (this.selectedCgroups.has(cgroupName)) {
            return this.colorMap.get(cgroupName) || null;
        }

        return null;
    }
}

// Create a singleton instance to share across the application
const globalColorMapper = new ColorMapper();
export default globalColorMapper;
