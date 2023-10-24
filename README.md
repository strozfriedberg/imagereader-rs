# [Expert Witness Compression Format (EWF)](https://github.com/libyal/libewf/blob/main/documentation/Expert%20Witness%20Compression%20Format%20(EWF).asciidoc)

### Supported file formats:

* EWF
* EWF-E01
* EWF-S01
* EWF-L01

### Supported features:
* multiple segments (files)
* chunk decompression (zlib)
* checking all checksums


## TODO
* [EWF2](https://github.com/libyal/libewf/blob/main/documentation/Expert%20Witness%20Compression%20Format%202%20(EWF2).asciidoc)

Sample of usage:
```
    use e01::e01_reader::E01Reader;

    fn read_e01(e01_path: &str) {
        let e01_reader = E01Reader::open(&e01_path).unwrap();

        let mut buf: Vec<u8> = vec![0; 1048576];
        let mut offset = 0;
        while offset < e01_reader.total_size {
            let readed = e01_reader.read_at_offset(offset, &mut buf).unwrap();
            if readed == 0 {
                break;
            }

            // process buf[..readed]

            offset += readed;
        }
    }

```