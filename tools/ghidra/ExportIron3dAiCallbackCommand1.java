// Emits the command-one consumer reached from the recovered CreateSuperAI
// host callback. Run headless; the original PE remains read only.
import ghidra.app.decompiler.DecompInterface;
import ghidra.app.script.GhidraScript;
import ghidra.program.model.address.Address;
import ghidra.program.model.listing.Function;

public class ExportIron3dAiCallbackCommand1 extends GhidraScript {
    private static final long ADDRESS = 0x10095160L;

    @Override
    public void run() throws Exception {
        Address address = currentProgram.getAddressFactory().getDefaultAddressSpace()
            .getAddress(ADDRESS);
        Function function = currentProgram.getFunctionManager().getFunctionAt(address);
        println("===== Iron3D CreateSuperAI callback command 1 =====");
        if (function == null) { println("missing"); return; }
        DecompInterface decompiler = new DecompInterface();
        decompiler.openProgram(currentProgram);
        println(decompiler.decompileFunction(function, 120, monitor).getDecompiledFunction().getC());
        decompiler.dispose();
    }
}
