import { describe, expect, it, vi } from 'vitest';

// `convertFileSrc` needs a Tauri host, so it is stubbed to return its input.
// What is under test is the path handed to it, which is the half that has
// actually been wrong before.
vi.mock('@tauri-apps/api/core', () => ({ convertFileSrc: (p: string) => p }));

const { shotUrl, shotThumbUrl } = await import('./shots');

describe('screenshot URLs', () => {
  it('points into the Screenshots subfolder, not the clips folder', () => {
    // A screenshot beside the clips would collide with a clip's own {id}.jpg
    // thumbnail. The subfolder prevents that, and it has to be in the URL or
    // the tab renders clip thumbnails instead.
    expect(shotUrl('D:\\Videos\\Trix', '20260827_143012')).toBe(
      'D:\\Videos\\Trix\\Screenshots\\20260827_143012.jpg',
    );
    expect(shotThumbUrl('D:\\Videos\\Trix', '20260827_143012')).toBe(
      'D:\\Videos\\Trix\\Screenshots\\20260827_143012.thumb.jpg',
    );
  });

  it('strips a trailing separator, including on a drive root', () => {
    // `clip_dir` is a config key a person types. A drive root carries a
    // trailing separator by necessity, and the doubled separator that results
    // can miss the asset-scope glob -- a broken image with nothing logged.
    expect(shotUrl('D:\\', '20260827_143012')).toBe('D:\\Screenshots\\20260827_143012.jpg');
    expect(shotUrl('D:\\Videos\\Trix\\', '20260827_143012')).toBe(
      'D:\\Videos\\Trix\\Screenshots\\20260827_143012.jpg',
    );
  });
});
