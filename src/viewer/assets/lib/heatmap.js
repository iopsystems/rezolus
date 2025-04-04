export function heatmapPlugin(config) {
    return {
        hooks: {
            // Draw the heatmap
            draw: u => {
                const { ctx, data } = u;
                        
                const [xValues, yValues, zValues] = data;
                const cols = xValues.length;
                const rows = yValues.length;
                        
                // Calculate visible data boundaries
                const xMin = u.scales.x.min;
                const xMax = u.scales.x.max;
                const yMin = u.scales.y.min;
                const yMax = u.scales.y.max;
                console.log(u.scales);
                        
                // Calculate visible indices
                const iMin = Math.max(0, Math.floor(yMin));
                const iMax = Math.min(rows - 1, Math.ceil(yMax));
                const jMin = Math.max(0, Math.floor(xMin));
                const jMax = Math.min(cols - 1, Math.ceil(xMax));
                        
                // Calculate cell dimensions based on visible area
                let cellWidth = u.bbox.width / (xMax - xMin);
                let cellHeight = u.bbox.height / (yMax - yMin);
                        
                // Enforce minimum cell size
                const minCellSize = config.minCellSize;
                        
                // Get color for a value
                function getColor(value) {
                    const paletteIdx = Math.min(
                        config.colorPalette.length - 1,
                        Math.floor(value * config.colorPalette.length)
                    );
                    return config.colorPalette[paletteIdx];
                }
                        
                // Save context state
                ctx.save();
                        
                // Set clip region to plot area
                ctx.beginPath();
                ctx.rect(u.bbox.left, u.bbox.top, u.bbox.width, u.bbox.height);
                ctx.clip();
                        
                // Draw each visible cell
                for (let i = iMin; i <= iMax; i++) {
                    for (let j = jMin; j <= jMax; j++) {
                        const xPos = u.valToPos(j, 'x', true);
                        const yPos = u.valToPos(i, 'y', true);
                        const zIdx = i * cols + j;
                        const value = zValues[zIdx];
                                
                        if (value === null) continue;
                                
                        // Calculate cell dimensions, ensuring they're at least minCellSize pixels
                        const nextXPos = u.valToPos(j + 1, 'x', true);
                        const nextYPos = u.valToPos(i + 1, 'y', true);
                                
                        let w = Math.abs(nextXPos - xPos);
                        let h = Math.abs(nextYPos - yPos);
                                
                        // If cell is too small, use minimum size instead
                        w = Math.max(w, minCellSize);
                        h = Math.max(h, minCellSize);
                                
                        // Fill cell with color based on value
                        ctx.fillStyle = getColor(value);
                        ctx.fillRect(
                            xPos - w / 2 - 0.5,
                            yPos - h / 2 - 0.5,
                            w + 0.5,
                            h + 0.5,
                        );
                    }
                }
                        
                // Restore context state
                ctx.restore();
            },
        }
    };
}