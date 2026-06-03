// License is MIT: https://opensource.org/license/mit

export class RasterPlot {
  // Create the stable DOM and canvas state that all later view and raster updates reuse.
  constructor(container, { view, raster, palette = PALETTE }) {
    // Initialize containing div, which bounds the visible extent of the canvas element
    this.el =
      container.querySelector("div") ??
      container.appendChild(document.createElement("div"));
    this.el.style.position = "relative";
    this.el.style.overflow = "hidden";

    // Initialize the canvas, which visualizes the raster data
    this.canvas =
      this.el.querySelector("canvas") ??
      this.el.appendChild(document.createElement("canvas"));
    this.canvas.style.position = "absolute";
    this.canvas.style.transformOrigin = "0 0";
    this.canvas.style.imageRendering = "pixelated";

    this.palette = palette;
    this.view = view;
    this.raster = raster;

    // The turbo palette below is authored in sRGB, so the canvas must decode the
    // bytes as sRGB. (A display-p3 context would reinterpret the same numbers in a
    // wider gamut and render the colormap oversaturated.)
    this.ctx = this.canvas.getContext("2d", {
      alpha: true,
      colorSpace: "srgb",
    });
    this.ctx.imageSmoothingEnabled = false;

    this.setView(view);
    this.setRaster(raster);
  }

  // Resize the viewport shell and recompute where the current raster belongs on screen.
  setView(view) {
    this.view = view;
    this.el.style.width = this.view.size.width + "px";
    this.el.style.height = this.view.size.height + "px";
    this.#update();
  }

  // Swap in a new raster payload, resize the backing canvas if needed, and redraw it.
  setRaster(raster) {
    this.raster = raster;
    const { width, height } = this.raster.size;
    if (this.canvas.width !== width) this.canvas.width = width;
    if (this.canvas.height !== height) this.canvas.height = height;
    this.#update();
    this.#draw();
  }

  // Recolor the current raster data without changing its world placement.
  setPalette(palette) {
    this.palette = palette;
    this.#update();
    this.#draw();
  }

  // Tear down the renderer's DOM when the host component is leaving the page.
  destroy() {
    this.el.remove();
  }

  // Project the raster's world-space extent into the current viewport and update the canvas transform.
  #update() {
    const view = this.view;
    const raster = this.raster;

    const placement = {
      originX:
        ((raster.extent.x0 - view.extent.x0) /
          (view.extent.x1 - view.extent.x0)) *
        view.size.width,
      originY:
        ((view.extent.y1 - raster.extent.y0) /
          (view.extent.y1 - view.extent.y0)) *
        view.size.height,
      scaleX:
        ((raster.extent.x1 - raster.extent.x0) /
          (view.extent.x1 - view.extent.x0) /
          raster.size.width) *
        view.size.width,
      scaleY: -(
        ((raster.extent.y1 - raster.extent.y0) /
          (view.extent.y1 - view.extent.y0) /
          raster.size.height) *
        view.size.height
      ),
    };

    this.canvas.style.transform =
      `translate(${placement.originX}px, ${placement.originY}px) ` +
      `scale(${placement.scaleX}, ${placement.scaleY})`;
  }

  // Decode indexed raster bytes into RGBA pixels and paint the backing canvas.
  #draw() {
    const { width, height } = this.raster.size;
    const bytes = decodeBase64(this.raster.data);
    if (bytes.length !== width * height) {
      throw Error("RasterPlot: raster length does not match raster size");
    }
    const img = this.ctx.createImageData(width, height);
    const pixels = new Uint32Array(img.data.buffer);
    for (let i = 0; i < bytes.length; i++) {
      pixels[i] = this.palette[bytes[i]];
    }
    this.ctx.putImageData(img, 0, 0);
  }
}

// Turn the serialized raster payload back into raw palette-index bytes.
function decodeBase64(str) {
  const s = atob(str);
  const out = new Uint8Array(s.length);
  for (let i = 0; i < s.length; i++) out[i] = s.charCodeAt(i);
  return out;
}

// 256-entry turbo colormap, ABGR u32 byte order
const PALETTE = new Uint32Array([
  4279965475, 4280818215, 4281539627, 4282326575, 4283047986, 4283704118,
  4284425529, 4285015867, 4285671998, 4286262336, 4286787394, 4287312196,
  4287837253, 4288362054, 4288821576, 4289215561, 4289674825, 4290068810,
  4290462794, 4290790987, 4291119435, 4291447883, 4291776075, 4292038987,
  4292301898, 4292564554, 4292761929, 4293024841, 4293222216, 4293353799,
  4293551174, 4293683013, 4293814852, 4293946692, 4294078274, 4294144577,
  4294276416, 4294342719, 4294408766, 4294409533, 4294475836, 4294542138,
  4294542649, 4294543416, 4294544183, 4294544694, 4294479925, 4294480691,
  4294415666, 4294416433, 4294351664, 4294286639, 4294221870, 4294156845,
  4294026540, 4293961515, 4293896746, 4293766186, 4293701417, 4293570856,
  4293440552, 4293309991, 4293244967, 4293114662, 4292984102, 4292853542,
  4292657445, 4292526885, 4292396581, 4292266021, 4292069925, 4291939365,
  4291808805, 4291612710, 4291482150, 4291351590, 4291155495, 4291024935,
  4290828584, 4290698025, 4290501929, 4290305834, 4290175019, 4289978924,
  4289848109, 4289652014, 4289521200, 4289325105, 4289128754, 4288998196,
  4288801845, 4288671031, 4288474937, 4288344122, 4288147772, 4288016958,
  4287820608, 4287689794, 4287493444, 4287362630, 4287166280, 4287035210,
  4286904397, 4286708047, 4286577233, 4286446164, 4286249814, 4286118745,
  4285987932, 4285856862, 4285660513, 4285529444, 4285398374, 4285267305,
  4285136236, 4285005423, 4284874354, 4284743285, 4284612216, 4284481147,
  4284349822, 4284284289, 4284153220, 4284022151, 4283890826, 4283825293,
  4283693968, 4283562899, 4283497110, 4283366041, 4283300252, 4283168927,
  4283103394, 4282972070, 4282906281, 4282840492, 4282709167, 4282643378,
  4282577589, 4282511800, 4282380475, 4282314686, 4282248641, 4282182851,
  4282117062, 4282051017, 4281985228, 4281919439, 4281853393, 4281787348,
  4281721559, 4281655513, 4281589724, 4281523678, 4281523168, 4281457123,
  4281391077, 4281325031, 4281324777, 4281258732, 4281192686, 4281126640,
  4281125873, 4281059827, 4281059317, 4280993271, 4280927224, 4280926458,
  4280860411, 4280859901, 4280793598, 4280793087, 4280727039, 4280726271,
  4280660223, 4280659455, 4280593407, 4280592639, 4280526591, 4280525823,
  4280459519, 4280459007, 4280392703, 4280392191, 4280391423, 4280325119,
  4280324351, 4280258303, 4280257535, 4280191231, 4280190463, 4280124415,
  4280123647, 4280122879, 4280056575, 4280056063, 4279989759, 4279988991,
  4279922687, 4279921917, 4279855868, 4279855098, 4279788793, 4279788023,
  4279787510, 4279721204, 4279654898, 4279654384, 4279588078, 4279587308,
  4279521258, 4279520488, 4279454182, 4279453668, 4279387362, 4279386847,
  4279320541, 4279254490, 4279253720, 4279187669, 4279187155, 4279120848,
  4279054798, 4279054283, 4278988233, 4278921926, 4278921411, 4278855361,
  4278789310, 4278789051, 4278723001, 4278656950, 4278656436, 4278590385,
  4278524591, 4278524076, 4278458282, 4278458024, 4278391973, 4278326179,
  4278325921, 4278260127, 4278194333, 4278194075, 4278193818, 4278193816,
  4278193558, 4278193301, 4278193300, 4278193299, 4278193298, 4278193041,
  4278193297, 4278193296, 4278193296, 4278193296,
]);
