class PcmCaptureProcessor extends AudioWorkletProcessor {
  constructor() {
    super();
    this.targetRate = 16000;
    this.cursor = 0;
    this.pending = [];
  }

  process(inputs) {
    const channel = inputs[0] && inputs[0][0];
    if (!channel || channel.length === 0) {
      return true;
    }

    let sum = 0;
    for (let i = 0; i < channel.length; i += 1) {
      sum += channel[i] * channel[i];
    }
    const rms = Math.sqrt(sum / channel.length);
    const ratio = sampleRate / this.targetRate;
    let cursor = this.cursor;

    while (cursor < channel.length) {
      const index = Math.floor(cursor);
      const next = Math.min(index + 1, channel.length - 1);
      const frac = cursor - index;
      const sample = channel[index] * (1 - frac) + channel[next] * frac;
      this.pending.push(sample);
      cursor += ratio;
    }
    this.cursor = cursor - channel.length;

    if (this.pending.length >= 640) {
      const pcm = new Int16Array(this.pending.length);
      for (let i = 0; i < this.pending.length; i += 1) {
        const sample = Math.max(-1, Math.min(1, this.pending[i]));
        pcm[i] = sample < 0 ? sample * 0x8000 : sample * 0x7fff;
      }
      const buffer = pcm.buffer;
      this.pending = [];
      this.port.postMessage({ pcm: buffer, rms }, [buffer]);
    } else {
      this.port.postMessage({ rms });
    }
    return true;
  }
}

registerProcessor("pcm-capture", PcmCaptureProcessor);
