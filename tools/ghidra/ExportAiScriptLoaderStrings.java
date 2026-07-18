// Emits path literals used by the GOG AI script loader at 0x10001000.
// Run headless; the original PE remains read only.
import ghidra.app.script.GhidraScript;
import ghidra.program.model.address.Address;

public class ExportAiScriptLoaderStrings extends GhidraScript {
    private static final long[] ADDRESSES = {
        0x10038a88L, 0x10038a9cL, 0x10038aa4L, 0x10038aacL,
        0x10038ab4L, 0x10038abcL, 0x10038ad0L, 0x10038ad8L,
        0x10038ae0L
    };

    @Override
    public void run() throws Exception {
        for (long value : ADDRESSES) {
            Address address = currentProgram.getAddressFactory().getDefaultAddressSpace()
                .getAddress(value);
            byte[] bytes = new byte[256];
            int count = currentProgram.getMemory().getBytes(address, bytes);
            StringBuilder text = new StringBuilder();
            for (int index = 0; index < count && bytes[index] != 0; index++) {
                text.append((char) (bytes[index] & 0xff));
            }
            println(address + " = \"" + text + "\"");
        }
    }
}
