/**
 * ColorMapper provides consistent color assignment for cgroups across all charts and page refreshes
 * using a deterministic hash function to assign colors based on cgroup names.
 * 
 * Can also be configured (useConsistentCgroupColors == false) to use a more distinguishable (but inconsistent)
 * color palette. This assigns colors to each series based on its index rather than its name.
 * 
 * In all cases, the "Other" category is always gray.
 */
export class ColorMapper {
    constructor() {
        // Store color assignments for cgroups
        this.colorMap = new Map();
        // Default mode: consistent cgroup colors
        this.useConsistentCgroupColors = true;

        /*
            import colorsys

            count = 45
            for i in range(count):
                h = i / count
                if i % 3 == 0:
                    l = .5
                    s = .5
                elif i % 3 == 1:
                    l = .6
                    s = .6
                else:
                    l = .4
                    s = .7
                    
                (r, g, b) = colorsys.hls_to_rgb(h, l, s)
                x = ''.join('{:02X}'.format(round(a * 255)) for a in [r, g, b])
                print("'#" + x + "',")
        */
        this.colorPalette = [
            '#BF4040',
            '#D66C5C',
            '#AD451F',
            '#BF7340',
            '#D69D5C',
            '#AD7E1F',
            '#BFA640',
            '#D6CE5C',
            '#A4AD1F',
            '#A6BF40',
            '#ADD65C',
            '#6BAD1F',
            '#73BF40',
            '#7CD65C',
            '#32AD1F',
            '#40BF40',
            '#5CD66C',
            '#1FAD45',
            '#40BF73',
            '#5CD69D',
            '#1FAD7E',
            '#40BFA6',
            '#5CD6CE',
            '#1FA4AD',
            '#40A6BF',
            '#5CADD6',
            '#1F6BAD',
            '#4073BF',
            '#5C7CD6',
            '#1F32AD',
            '#4040BF',
            '#6C5CD6',
            '#451FAD',
            '#7340BF',
            '#9D5CD6',
            '#7E1FAD',
            '#A640BF',
            '#CE5CD6',
            '#AD1FA4',
            '#BF40A6',
            '#D65CAD',
            '#AD1F6B',
            '#BF4073',
            '#D65C7C',
            '#AD1F32',
        ]

        // Always use gray for "Other" category - consistent across all charts
        this.otherColor = '#666666';
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
        // This results in a 15-color palette (only the most saturated of the original 45 colors) in a reasonable order.
        const STARTING_INDEX = 2;
        const JUMP_DISTANCE = 12;
        return this.colorPalette[(STARTING_INDEX + index * JUMP_DISTANCE) % this.colorPalette.length];
    }

    /**
     * Get colors for an array of cgroup names
     * @param {string[]} cgroupNames - Array of cgroup names
     * @returns {string[]} Array of color codes in the same order
     */
    getColors(cgroupNames) {
        return cgroupNames.map((name, index) => this.useConsistentCgroupColors || name === "Other" ? this.getColorByName(name) : this.getColorByIndex(index));
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
    }

    setUseConsistentCgroupColors(useConsistentCgroupColors) {
        this.useConsistentCgroupColors = useConsistentCgroupColors;
    }

    getUseConsistentCgroupColors() {
        return this.useConsistentCgroupColors;
    }
}

// Create a singleton instance to share across the application
const globalColorMapper = new ColorMapper();
export default globalColorMapper;